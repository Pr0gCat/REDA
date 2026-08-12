# Spring Placement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace "rows by logic depth, columns by barycentre" with a
continuous spring relaxation, legalised onto the lattice afterwards, and route
`compile()` through it.

**Architecture:** `primitive_graph::expand` already produces primitives and
typed edges. Two new stages sit between it and routing: `relax` solves a
quadratic spring system exactly for the current facings, chooses each body's
best facing among four, and projects the spacing rule as a hard constraint --
repeating until nothing moves. `snap` rounds the result onto the lattice and
collapses each gate's bodies back to the one anchor `PlanCandidate` has room
for. Routing, realisation and the four invariants are untouched.

**Tech Stack:** Rust 2021, no new dependencies. Existing `PrimitiveGraph`,
`physical::variants`, `World`, `PlanCandidate`, `route_every_net`,
`realise_and_verify`, and `./check.sh`.

**Spec:** `docs/superpowers/specs/2026-08-13-spring-placement.md`. Read it
before Task 1; this plan implements it and does not restate its reasoning.

## Global Constraints

- Minecraft Java **26.2** is the only product target.
- **No new dependencies.** The crate has none for linear algebra and compiles
  to wasm; the solver is sixty lines of dense Cholesky written here.
- **No unseeded randomness and no clock.** `RelaxEffort` carries an explicit
  `seed: u64`; everything else is deterministic.
- **`f64` is the solver's internal state, never an ordering key.**
  `2026-08-11-unified-3d-planner.md` forbids floating-point ordering; what
  leaves these modules is integer anchors and a facing index. Task 12 is the
  test that native and wasm agree.
- **Two conductors of different signals need 2 cells of clearance**, derived in
  `2026-08-09-channel-safety-condition.md` from `dust_reach`. Not a tuning
  parameter.
- **Every existing measurement must stay identical through Stage 0.**
  `the_hand_written_circuits_keep_their_measured_size` pins 472 / 1,784 /
  6,416 / 16,244 and must not move until Task 13.
- **`./check.sh` green after every task.** It builds the viewer and the wasm
  bundle too; a public enum gaining a variant has twice left the viewer
  unbuildable while the root crate reported clean.
- Comments and doc comments in English.

---

## File Structure

| File | Owns | Stage |
|---|---|---|
| `src/compile/geometry.rs` *(new)* | `CellFacing`, rotation, and where a gate cell's sockets and pin sit for a given facing. The one place the north assumption used to be six places. | 0 |
| `src/compile/mod.rs` | `place_nor_gate` / `place_merge_gate` / `place_primary_input` / `gate_footprint` take a facing; `CompiledCircuit` records what each gate was built as. | 0, 3 |
| `src/compile/equivalence.rs`, `world_partition.rs`, `routing_stats.rs` | Ask the recorded facing instead of assuming north. | 0 |
| `src/compile/topology.rs` | Footprint-area tables derived from `geometry` instead of tabulated. | 0 |
| `src/compile/relax/linear.rs` *(new)* | Dense Cholesky factorise-once/solve-many for one SPD system. Pure numerics, no domain knowledge. | 1 |
| `src/compile/relax/build.rs` *(new)* | Bodies, pulls and welds from a netlist and its primitive graph. Where junctions get re-inserted. | 1 |
| `src/compile/relax/project.rs` *(new)* | The separation rule and the welds, alternating, welds last. | 1, 2 |
| `src/compile/relax/snap.rs` *(new)* | Rounding onto the lattice, refusing an unconverged placement, collapsing bodies to gate anchors. | 1 |
| `src/compile/relax/mod.rs` *(new)* | `relax` itself: the step loop, facing enumeration, convergence, errors. | 1 |
| `src/compile/planner.rs` | `plan_from_netlist` places by relaxation; `Shape` removed. | 1, 2 |

`relax` is a directory module rather than one file because `planner.rs` is
already 182 KB and `mod.rs` 362 KB, and each of these five has one
responsibility and its own tests.

**It must live inside `src/compile/`.** `Route`'s fields are private, its only
public constructor leaves `realisation` and `floors` empty, and `emit_routes`
refuses a route in that state. Routing has to be delegated back to
`route_every_net`, which is `planner`'s.

---

# Stage 0 -- one place for a gate's geometry

No behaviour changes: every caller passes north, and every existing
measurement must come out identical. That is what makes this stage testable on
its own.

### Task 1: `CellFacing` and the rotation nobody wrote down

**Files:**
- Create: `src/compile/geometry.rs`
- Modify: `src/compile/mod.rs` (add `pub mod geometry;` beside `pub mod physical;` at line 58)
- Test: `src/compile/geometry.rs` (in-file `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `redstone::simulator::position::{Position, HORIZONTAL}`,
  `redstone::world::block::Facing`, `compile::physical::variants`,
  `compile::topology::Primitive`.
- Produces:
  - `pub struct CellFacing(u8)` with `CellFacing::NORTH`,
    `CellFacing::from_index(u8) -> Option<CellFacing>`,
    `CellFacing::index(self) -> u8`, `CellFacing::direction(self) -> Facing`,
    and `Default` returning north.
  - `pub fn rotate(offset: (i32, i32, i32), facing: CellFacing) -> (i32, i32, i32)`
  - `pub fn turn(direction: Facing, facing: CellFacing) -> Facing`
  - `pub fn input_directions(facing: CellFacing) -> [Facing; 3]`
  - `pub fn output_direction(facing: CellFacing) -> Facing`

- [ ] **Step 1: Write the failing tests**

Create `src/compile/geometry.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::physical;
    use crate::compile::topology::Primitive;
    use crate::redstone::world::block::BlockKind;

    /// North is what every gate this compiler has ever placed is built as, so
    /// north has to be the identity and the default or Stage 0 changes
    /// behaviour it promised not to.
    #[test]
    fn north_is_the_identity_and_the_default() {
        assert_eq!(CellFacing::default(), CellFacing::NORTH);
        assert_eq!(output_direction(CellFacing::NORTH), Facing::North);
        assert_eq!(
            input_directions(CellFacing::NORTH),
            [Facing::West, Facing::East, Facing::South]
        );
        assert_eq!(rotate((1, 2, 3), CellFacing::NORTH), (1, 2, 3));
    }

    /// The fourth horizontal face is the output's, whichever way the cell is
    /// turned -- which is the whole reason a gate takes at most three inputs.
    #[test]
    fn a_cell_never_takes_an_input_from_the_face_its_output_leaves() {
        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let out = output_direction(facing);
            assert!(
                !input_directions(facing).contains(&out),
                "{facing:?} takes an input from {out:?}, where its output goes"
            );
        }
    }

    /// Turning a cell twice by the same quarter turn is turning it half way
    /// round, which is the cheapest check that `rotate` is a rotation and not
    /// four hand-written tables that happen to look plausible.
    #[test]
    fn turning_east_twice_is_turning_south_once() {
        for offset in [(1, 0, 0), (0, 0, -1), (-1, 2, 3)] {
            let twice = rotate(rotate(offset, CellFacing::EAST), CellFacing::EAST);
            assert_eq!(twice, rotate(offset, CellFacing::SOUTH), "for {offset:?}");
        }
    }

    /// `physical.rs` declares its variant arrays in `HORIZONTAL` order and
    /// says so nowhere. This is that statement, made checkable: relaxation
    /// picks a facing by index, and an index that means something else would
    /// build a gate pointing the wrong way with nothing to catch it.
    #[test]
    fn every_variant_faces_the_facing_its_index_claims() {
        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let slot = usize::from(index);

            let torch = &physical::variants(Primitive::Torch)[slot];
            let torch_at = Position::new(0, 0, 0).offset(facing.direction());
            assert_eq!(
                torch.block_at(torch_at).kind,
                BlockKind::WallTorch,
                "variants(Torch)[{index}] has no torch at {torch_at:?}"
            );

            let repeater = &physical::variants(Primitive::Repeater)[slot];
            assert_eq!(
                repeater.block_at(Position::new(0, 0, 0)).facing,
                Some(facing.direction()),
                "variants(Repeater)[{index}] is not built facing {:?}",
                facing.direction()
            );
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::geometry`

Expected: compile failure -- `CellFacing`, `rotate`, `input_directions` and
`output_direction` do not exist, and `geometry` is not a declared module.

- [ ] **Step 3: Declare the module**

In `src/compile/mod.rs`, beside the existing `pub mod physical;` at line 58:

```rust
pub mod geometry;
```

- [ ] **Step 4: Write the implementation**

Prepend to `src/compile/geometry.rs`, above the test module:

```rust
//! One place for the geometry a gate's cell has: which faces its inputs
//! arrive on, which face its output leaves by, and which `physical` variant a
//! facing selects.
//!
//! Before this, that was six modules. Five named `INPUT_DIRECTIONS` and
//! `OUTPUT_DIRECTION`; the sixth, `topology`, hardcoded the consequence as
//! footprint-area tables with no symbol to grep for. None of them could have
//! been asked what a gate turned east looks like, because none of them could
//! be asked anything -- they were constants.

use crate::redstone::simulator::position::{Position, HORIZONTAL};
use crate::redstone::world::block::Facing;

/// One of the four horizontal orientations a gate cell can be built in.
///
/// A `u8` index rather than a [`Facing`], for two reasons. `Facing` has `Up`
/// and `Down`, and a gate cell turned onto its side is not a thing this
/// compiler can build. And `physical::variants` is a four-element array
/// indexed positionally, which is what this index indexes -- the linkage
/// `every_variant_faces_the_facing_its_index_claims` proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CellFacing(u8);

impl CellFacing {
    /// What every gate this compiler has placed so far is built as.
    pub const NORTH: CellFacing = CellFacing(0);
    pub const SOUTH: CellFacing = CellFacing(1);
    pub const EAST: CellFacing = CellFacing(2);
    pub const WEST: CellFacing = CellFacing(3);

    /// The facing `physical::variants`' `index`-th entry is built for, or
    /// `None` for an index no variant array has.
    pub fn from_index(index: u8) -> Option<CellFacing> {
        (usize::from(index) < HORIZONTAL.len()).then_some(CellFacing(index))
    }

    /// Which entry of `physical::variants` this selects.
    pub fn index(self) -> u8 {
        self.0
    }

    /// The compass direction this cell's output leaves in.
    pub fn direction(self) -> Facing {
        HORIZONTAL[usize::from(self.0)]
    }
}

/// `offset`, written for a north-facing cell, read on one turned to `facing`.
///
/// The turn is about Y, so heights are untouched: a torch's support stays
/// beside its torch and a repeater's floor stays under it whichever way the
/// pair is turned.
pub fn rotate(offset: (i32, i32, i32), facing: CellFacing) -> (i32, i32, i32) {
    let (x, y, z) = offset;
    match facing.direction() {
        Facing::North => (x, y, z),
        Facing::South => (-x, y, -z),
        Facing::East => (-z, y, x),
        Facing::West => (z, y, -x),
        // `CellFacing` indexes `HORIZONTAL`, which has neither.
        Facing::Up | Facing::Down => unreachable!("CellFacing is horizontal by construction"),
    }
}

/// `direction`, as read on a cell turned to `facing` from north.
pub fn turn(direction: Facing, facing: CellFacing) -> Facing {
    let unit = Position::new(0, 0, 0).offset(direction);
    match rotate((unit.x, unit.y, unit.z), facing) {
        (0, 0, -1) => Facing::North,
        (0, 0, 1) => Facing::South,
        (1, 0, 0) => Facing::East,
        (-1, 0, 0) => Facing::West,
        (0, 1, 0) => Facing::Up,
        (0, -1, 0) => Facing::Down,
        other => unreachable!("turning a unit vector gave {other:?}"),
    }
}

/// Every face a gate cell of this facing accepts an input on, in declared
/// input order.
///
/// Three, because the fourth horizontal face is the output's. Derived by
/// turning north's answer rather than tabulated per facing, so there is one
/// place to be wrong instead of four.
pub fn input_directions(facing: CellFacing) -> [Facing; 3] {
    const FACING_NORTH: [Facing; 3] = [Facing::West, Facing::East, Facing::South];
    FACING_NORTH.map(|direction| turn(direction, facing))
}

/// The face a gate cell of this facing sends its output out of.
pub fn output_direction(facing: CellFacing) -> Facing {
    facing.direction()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --release --lib compile::geometry`

Expected: 4 passed.

- [ ] **Step 6: Run the whole suite -- nothing else may move**

Run: `./check.sh`

Expected: `passed=470 failed=0 ignored=3` plus the four geometry tests, so
`passed=474`. Clippy clean, viewer green.

- [ ] **Step 7: Commit**

```bash
git add src/compile/geometry.rs src/compile/mod.rs && git commit -m "feat(geometry): a gate's facing is a thing that can be asked, not assumed"
```

---

### Task 2: A gate cell is built to a facing

**Files:**
- Modify: `src/compile/mod.rs` -- `place_nor_gate` (364-444), `place_merge_gate` (446-560), `place_primary_input` (~2476-2485), `gate_footprint` (6385-6459), and every caller
- Modify: `src/compile/planner.rs` -- `output_pin` (684-694), the seed loop's `gate_footprint` call (1888-1889), `emit_primitives`' two `place_*_gate` calls (613, 621)
- Test: `src/compile/mod.rs` (in-file `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `geometry::{CellFacing, input_directions, output_direction, rotate}` from Task 1.
- Produces:
  - `pub fn place_nor_gate(world: &mut World, origin: (i32, i32, i32), input_count: usize, facing: CellFacing) -> NorCell`
  - `pub fn place_merge_gate(world: &mut World, origin: (i32, i32, i32), input_count: usize, facing: CellFacing) -> NorCell`
  - `pub fn place_primary_input(world: &mut World, home: Position, facing: CellFacing) -> (Position, Position)`
  - `pub(crate) fn gate_footprint(origin: (i32, i32, i32), gate: &Gate, facing: CellFacing) -> (Vec<Anchor>, Vec<Anchor>, Anchor)`
  - `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` are **deleted**.

- [ ] **Step 1: Write the failing test**

Add to `src/compile/mod.rs`'s test module:

```rust
/// A cell built facing anywhere is the north cell turned, cell for cell.
///
/// This is the whole of Stage 0's claim. Relaxation will choose facings and
/// hand them here; if a turned cell were assembled from its own arithmetic
/// rather than from north's, the two would drift apart exactly where nobody
/// looks -- at the three facings no reference circuit uses yet.
#[test]
fn a_turned_gate_cell_is_the_north_one_turned() {
    use crate::compile::geometry::{self, CellFacing};

    let origin = (16, 1, 16);
    for arity in 1..=3usize {
        let mut north_world = World::new(40, 4, 40);
        let north = place_nor_gate(&mut north_world, origin, arity, CellFacing::NORTH);

        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let mut turned_world = World::new(40, 4, 40);
            let turned = place_nor_gate(&mut turned_world, origin, arity, facing);

            for (input, &offset) in north.input_offsets.iter().enumerate() {
                assert_eq!(
                    turned.input_offsets[input],
                    geometry::rotate(offset, facing),
                    "arity {arity} facing {index}: input {input}'s socket"
                );
            }
            assert_eq!(
                turned.output_offset,
                geometry::rotate(north.output_offset, facing),
                "arity {arity} facing {index}: output"
            );
            // Turning a rectangle swaps its sides; it does not change its area.
            let north_area = north.size.0 * north.size.2;
            assert_eq!(
                turned.size.0 * turned.size.2,
                north_area,
                "arity {arity} facing {index}: footprint area"
            );
        }
    }
}

/// A merge is built to the same footprint as a NOR of the same arity, and
/// stays that way turned -- which is what lets `emit`'s geometry stay
/// gate-kind-agnostic.
#[test]
fn a_turned_merge_keeps_a_nors_socket_faces() {
    use crate::compile::geometry::CellFacing;

    let origin = (16, 1, 16);
    for index in 0..4u8 {
        let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
        let mut nor_world = World::new(40, 4, 40);
        let mut merge_world = World::new(40, 4, 40);
        let nor = place_nor_gate(&mut nor_world, origin, 3, facing);
        let merge = place_merge_gate(&mut merge_world, origin, 3, facing);
        assert_eq!(nor.input_offsets, merge.input_offsets, "facing {index}");
        assert_eq!(merge.output_offset, (0, 0, 0), "facing {index}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --release --lib compile::tests::a_turned_gate_cell_is_the_north_one_turned`

Expected: compile failure -- `place_nor_gate` takes three arguments, not four.

- [ ] **Step 3: Take a facing in `place_nor_gate`**

In `src/compile/mod.rs`, replace the signature and the two direction uses.
Everything else in the function -- the bounding box, `NorCell` -- is unchanged:

```rust
pub fn place_nor_gate(
    world: &mut World,
    origin: (i32, i32, i32),
    input_count: usize,
    facing: geometry::CellFacing,
) -> NorCell {
    let inputs = geometry::input_directions(facing);
    assert!(
        input_count <= inputs.len(),
        "place_nor_gate takes at most {} inputs, got {input_count}",
        inputs.len()
    );

    let support = Position::new(origin.0, origin.1, origin.2);
    world.set(support.x, support.y, support.z, stone());

    let mut input_offsets = Vec::with_capacity(input_count);
    for &direction in inputs.iter().take(input_count) {
        let socket = support.offset(direction);
        input_offsets.push((
            socket.x - support.x,
            socket.y - support.y,
            socket.z - support.z,
        ));
    }

    let out = geometry::output_direction(facing);
    let output_torch_pos = support.offset(out);
    world.set(
        output_torch_pos.x,
        output_torch_pos.y,
        output_torch_pos.z,
        wall_torch(out),
    );

    let output_socket = output_torch_pos.offset(out);
```

- [ ] **Step 4: Take a facing in `place_merge_gate` and `place_primary_input`**

`place_merge_gate`'s change is the same shape -- `inputs` from
`geometry::input_directions(facing)`, `output_socket` from
`support.offset(geometry::output_direction(facing))`. Its `output_offset`
stays `(0, 0, 0)`: the junction is the cell, and turning it does not move it.

`place_primary_input` hardcodes `Facing::North` for its lever's pin
(mod.rs:2481). Replace:

```rust
    let pin = home.offset(geometry::output_direction(facing));
```

and add `facing: geometry::CellFacing` to its signature.

- [ ] **Step 5: Take a facing in `gate_footprint`**

```rust
pub(crate) fn gate_footprint(
    origin: (i32, i32, i32),
    gate: &Gate,
    facing: geometry::CellFacing,
) -> (Vec<Anchor>, Vec<Anchor>, Anchor) {
```

Inside it, pass `facing` to `place_merge_gate`/`place_nor_gate`, replace
`torch.offset(OUTPUT_DIRECTION)` with
`torch.offset(geometry::output_direction(facing))`, and replace the socket
loop's `INPUT_DIRECTIONS.iter()` with
`geometry::input_directions(facing).iter()`.

- [ ] **Step 6: Pass north at every call site, and delete the constants**

Every caller passes `geometry::CellFacing::NORTH`:

| Site | Call |
|---|---|
| `mod.rs:3738-3742` | `emit`'s per-gate placement |
| `mod.rs:4413` | `cell_geometry_by_input_count` |
| `mod.rs:6393-6397` | `gate_footprint`'s own two calls |
| `mod.rs:6472-6473` | `legacy_primitive_nodes` -> `gate_footprint` |
| `mod.rs:2476` region | `place_primary_input`'s callers |
| `planner.rs:613, 621` | `emit_primitives` |
| `planner.rs:1888-1889` | the seed loop's `gate_footprint` |
| `topology.rs:1799, 1811` | the footprint round-trip tests |

Then delete `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` (mod.rs:253-265). Every
remaining reference is in Task 3's modules and is fixed there; if the build
still names them after Task 3, a site was missed.

- [ ] **Step 7: `output_pin` takes a facing**

In `planner.rs:684-694`:

```rust
fn output_pin(
    world: &mut World,
    anchor: Anchor,
    cell: &compile::NorCell,
    facing: compile::geometry::CellFacing,
) -> (Position, Position) {
    let torch = Position::new(
        anchor.x + cell.output_offset.0,
        anchor.y + cell.output_offset.1,
        anchor.z + cell.output_offset.2,
    );
    let pin = torch.offset(compile::geometry::output_direction(facing));
    compile::ensure_floor(world, pin);
    world.set(pin.x, pin.y, pin.z, compile::dust());
    (torch, pin)
}
```

`emit_primitives` passes `compile::geometry::CellFacing::NORTH` at both call
sites for now; Task 8 gives it the candidate's recorded facing.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::tests::a_turned`

Expected: 2 passed.

- [ ] **Step 9: Prove nothing moved**

Run: `./check.sh`

Expected: `failed=0`, and `the_hand_written_circuits_keep_their_measured_size`
still pinning 472 / 1,784 / 6,416 / 16,244. If any of those four numbers
changed, a call site was given a facing other than north.

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(compile): a gate cell is built to a facing, and north is one of four"
```

---

### Task 3: A compiled circuit records which way each gate was built

Three modules read a finished world and re-derive each gate's sockets from a
constant. They cannot keep doing that once facings vary, and they cannot read
the answer back off the world either: a merge's junction is dust, and dust does
not say which of its four faces its route was meant to leave from. So the
facing is recorded where it is chosen, which is what this codebase already does
for `Route::realisation` and `RouteTerminal::repeaters`.

**Files:**
- Modify: `src/compile/mod.rs` -- `CompiledCircuit` (the struct and both construction sites at 6327-6334 and 6353-6360), `bypass_source_start` (1162-1169), `emit`'s pin loop (3767-3786), `resolve_directed_dust_terminals` (~4353), `source_pin_position` (4423-4442), `merge_gate_body_owners` (~4909)
- Modify: `src/compile/equivalence.rs` -- import (79), `verify_gate_structure` (400-482), `verify_merge_gate_structure` (490-588), `verify_lamp` (621-658)
- Modify: `src/compile/world_partition.rs` -- import (74), `check_gate_input_arity_agrees` (282-355), `resolve_node_position` (378-439)
- Modify: `src/compile/routing_stats.rs` -- import (39), `source_pin` (360-371)
- Modify: `src/compile/planner.rs` -- `route_in_order`'s two socket derivations (2089-2100, 2131-2148)
- Test: `src/compile/mod.rs`

**Interfaces:**
- Consumes: `geometry::{CellFacing, input_directions, output_direction}`.
- Produces:
  - `CompiledCircuit` gains `pub gate_facings: Vec<CellFacing>`, indexed by gate
    index, one entry per `netlist.gates`.
  - `pub(crate) fn gate_sockets(origin: Anchor, arity: usize, facing: CellFacing) -> Vec<Anchor>`
    in `geometry` -- the sockets a gate at `origin` occupies, in declared input
    order. Every module above calls this instead of writing the offset loop a
    seventh time.

- [ ] **Step 1: Write the failing test**

Add to `src/compile/mod.rs`'s test module:

```rust
/// Every compiled circuit says which way it built each gate, and today the
/// answer is north for all of them.
///
/// The value is dull; having somewhere to put it is not. Three modules verify
/// a world by recomputing where a gate's sockets must be, and once relaxation
/// turns gates they need to be told rather than to assume -- and a merge's
/// junction is dust, which cannot be asked.
#[test]
fn a_compiled_circuit_records_a_facing_for_every_gate() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::geometry::CellFacing;

    let (netlist, _) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 compiles");

    assert_eq!(compiled.gate_facings.len(), netlist.gates.len());
    assert!(
        compiled.gate_facings.iter().all(|&facing| facing == CellFacing::NORTH),
        "nothing chooses a facing yet, so every gate must still be north"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --release --lib compile::tests::a_compiled_circuit_records_a_facing_for_every_gate`

Expected: compile failure -- `CompiledCircuit` has no field `gate_facings`.

- [ ] **Step 3: Add `gate_sockets` to `geometry`**

```rust
/// The cells a gate at `origin` accepts its declared inputs in, in declared
/// input order.
///
/// Six modules used to compute this, each from the same constant and each
/// slightly differently -- one off a support, one off a junction, one from a
/// `NorCell`'s `input_offsets`. They are the same cells.
pub fn gate_sockets(origin: Position, arity: usize, facing: CellFacing) -> Vec<Position> {
    input_directions(facing)
        .iter()
        .take(arity)
        .map(|&direction| origin.offset(direction))
        .collect()
}
```

- [ ] **Step 4: Record the facing**

In `src/compile/mod.rs`, add to `CompiledCircuit`:

```rust
    /// Which way each gate's cell was built, by gate index.
    ///
    /// Recorded by whoever placed it, never read back off the world: a merge's
    /// junction is dust, and dust has no facing to read. A verifier that
    /// re-derives a gate's socket faces has to be told which faces those are.
    pub gate_facings: Vec<geometry::CellFacing>,
```

Both construction sites fill it with north:

```rust
        gate_facings: vec![geometry::CellFacing::NORTH; netlist.gates.len()],
```

- [ ] **Step 5: Make the three readers ask**

Each of these has the gate index in hand already.

`equivalence::verify_gate_structure` (457) and `verify_merge_gate_structure`
(556) -- take `facing: CellFacing` as a parameter, passed by the caller from
`compiled.gate_facings[g]`, and replace `INPUT_DIRECTIONS.iter()` with
`geometry::input_directions(facing).iter()`.

`equivalence::verify_lamp` (641) -- take the producing gate's facing and
replace:

```rust
    let expected = Position::new(tx, ty, tz)
        .offset(geometry::output_direction(facing))
        .down();
```

`world_partition::check_gate_input_arity_agrees` (309, 331) -- same
substitution, twice, using `compiled.gate_facings[g]`.

`world_partition::resolve_node_position` (427-437) -- an `IsolatingRepeater`'s
cell:

```rust
            let facing = compiled.gate_facings[*gate];
            let sockets = geometry::gate_sockets(junction, netlist.gates[*gate].inputs.len(), facing);
            sockets.get(*index).copied().ok_or_else(|| {
                PartitionError::CannotResolveNodePosition {
                    detail: format!("gate `{gate_name}`'s own branch {index} has no socket"),
                }
            })
```

`routing_stats::source_pin` (360-371) -- both arms take a facing; the lever arm
stops hardcoding `Facing::North`:

```rust
fn source_pin(netlist: &Netlist, compiled: &CompiledCircuit, source: Source) -> Position {
    match source {
        Source::Lever(i) => {
            let (x, y, z) = compiled.input_positions[&netlist.inputs[i]];
            Position::new(x, y, z).offset(geometry::output_direction(CellFacing::NORTH))
        }
        Source::Gate(g) => {
            let (x, y, z) = compiled.gate_output_positions[&netlist.gates[g].output];
            Position::new(x, y, z).offset(geometry::output_direction(compiled.gate_facings[g]))
        }
    }
}
```

A lever's facing is north until Stage 1 places levers too; leave it named
rather than bare so the next reader sees a decision instead of a constant.

- [ ] **Step 6: Make `mod.rs`'s own five sites ask**

`bypass_source_start`, `emit`'s pin loop, `resolve_directed_dust_terminals`,
`source_pin_position` and `merge_gate_body_owners` all sit inside the legacy
emitter, which builds every gate north. Each takes the facing from the gate
index it already has, via a local `let facing = geometry::CellFacing::NORTH;`
at the top of `emit` threaded through -- or, where the function has no gate
index, an added parameter. The mechanical rule: no call to
`geometry::input_directions` or `geometry::output_direction` may pass a literal
`CellFacing::NORTH` from inside a function that knows a gate index.

- [ ] **Step 7: Make `route_in_order` ask**

`planner.rs:2089-2100` and `2131-2148` derive a socket from
`compile::INPUT_DIRECTIONS[input_index]`, which no longer exists. Both become:

```rust
            let facing = candidate.facing_of(gate);
            let socket = step(support, compile::geometry::input_directions(facing)[input_index]);
```

with `PlanCandidate::facing_of(&self, node: usize) -> CellFacing` reading
`variant_indices` -- a field the struct already has and nothing has ever read:

```rust
    /// Which way node `node`'s cell is built.
    pub fn facing_of(&self, node: usize) -> geometry::CellFacing {
        self.variant_indices
            .get(node)
            .and_then(|&index| geometry::CellFacing::from_index(index))
            .unwrap_or_default()
    }
```

Note the disagreement this exposes and does not fix: `try_move`'s
`terminal_socket` (planner.rs:1559) picks a socket by geometry -- the
source-to-support delta -- while `route_in_order` picks by declared input
index. They can disagree today. Stage 1's Task 10 is where that has to be
settled; leave it alone here, and leave a comment saying so.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::tests::a_compiled_circuit_records_a_facing_for_every_gate`

Expected: PASS.

- [ ] **Step 9: Prove nothing moved**

Run: `./check.sh`

Expected: `failed=0`, the four pinned sizes unchanged. `INPUT_DIRECTIONS` and
`OUTPUT_DIRECTION` no longer exist anywhere:

```bash
git grep -n "INPUT_DIRECTIONS\|OUTPUT_DIRECTION" -- src tests viewer
```

Expected: no matches outside comments. Comments that explain the history may
keep the names; code may not.

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(compile): a verifier is told a gate's facing rather than assuming it"
```

---

### Task 4: The footprint tables are derived, not tabulated

`topology.rs` is the sixth module that assumes north, and the only one with no
symbol to grep for: it hardcodes the *consequence* as areas, `1 => 6, 2 => 9,
3 => 12` for a NOR and `2 => 6, 3 => 9` for a merge. A round-trip test pins
them to what `place_nor_gate` really builds, so they are correct today and
would silently stay correct-looking while meaning something else.

**Files:**
- Modify: `src/compile/topology.rs` -- `nor_footprint_area` (1274-1281), `merge_footprint_area` (1309-1315)
- Test: `src/compile/topology.rs` (the existing round-trip tests at 1788-1818 are the test; they must keep passing unchanged)

**Interfaces:**
- Consumes: `geometry::{CellFacing, input_directions, output_direction}`.
- Produces: no new public API. Both functions keep their signatures and their
  answers.

- [ ] **Step 1: Write the failing test**

Add to `topology.rs`'s test module:

```rust
/// A cell's footprint area is what it is whichever way the cell is turned:
/// turning a rectangle swaps its sides.
///
/// The tables this replaces could not have been asked. They were three
/// numbers that happened to be right for north.
#[test]
fn footprint_area_does_not_depend_on_facing() {
    use crate::compile::geometry::CellFacing;

    for index in 0..4u8 {
        let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
        for arity in 1..=3usize {
            assert_eq!(
                nor_footprint_area_facing(arity, facing),
                nor_footprint_area(arity),
                "NOR arity {arity} facing {index}"
            );
        }
        for arity in 2..=3usize {
            assert_eq!(
                merge_footprint_area_facing(arity, facing),
                merge_footprint_area(arity),
                "merge arity {arity} facing {index}"
            );
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --release --lib compile::topology::tests::footprint_area_does_not_depend_on_facing`

Expected: compile failure -- `nor_footprint_area_facing` does not exist.

- [ ] **Step 3: Derive the area**

```rust
/// The ground-plan area a gate cell occupies, computed from where its cells
/// are rather than tabulated.
///
/// A cell is its origin, one socket per declared input, and its outbound pin
/// `pin_hops` out along its output face -- two for a NOR, whose torch stands
/// between origin and pin, one for a merge, whose junction *is* the origin.
fn footprint_area(arity: usize, facing: geometry::CellFacing, pin_hops: i32) -> usize {
    let origin = Position::new(0, 0, 0);
    let mut min = (origin.x, origin.z);
    let mut max = (origin.x, origin.z);
    let mut extend = |p: Position| {
        min = (min.0.min(p.x), min.1.min(p.z));
        max = (max.0.max(p.x), max.1.max(p.z));
    };

    for socket in geometry::gate_sockets(origin, arity, facing) {
        extend(socket);
    }
    let out = geometry::output_direction(facing);
    let mut pin = origin;
    for _ in 0..pin_hops {
        pin = pin.offset(out);
    }
    extend(pin);

    ((max.0 - min.0 + 1) * (max.1 - min.1 + 1)) as usize
}

fn nor_footprint_area_facing(arity: usize, facing: geometry::CellFacing) -> usize {
    footprint_area(arity, facing, 2)
}

fn merge_footprint_area_facing(arity: usize, facing: geometry::CellFacing) -> usize {
    footprint_area(arity, facing, 1)
}

fn nor_footprint_area(arity: usize) -> usize {
    nor_footprint_area_facing(arity, geometry::CellFacing::NORTH)
}

fn merge_footprint_area(arity: usize) -> usize {
    merge_footprint_area_facing(arity, geometry::CellFacing::NORTH)
}
```

Keep both `nor_footprint_area` and `merge_footprint_area` at their existing
signatures so their callers are untouched. Delete the `match` tables and move
their doc comments -- the derivation prose at topology.rs:1283-1308 is right
and is now what the code does.

- [ ] **Step 4: Run the tests**

Run: `cargo test --release --lib compile::topology`

Expected: PASS, including the pre-existing round-trip tests at 1788-1818 that
compare these answers against a really-placed cell's `size`. Those are what
prove the derivation reproduces 6/9/12 and 6/9.

- [ ] **Step 5: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`, four pinned sizes unchanged.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(topology): a footprint area is measured, not remembered"
```

**Stage 0 is done when** `git grep INPUT_DIRECTIONS` finds nothing but prose,
every gate cell takes a facing, `CompiledCircuit` records one per gate, and
`the_hand_written_circuits_keep_their_measured_size` still pins 472 / 1,784 /
6,416 / 16,244.

---

# Stage 1 -- relaxation and snapping, one plane

Bodies stay at the Y their starting layout gave them. `plan_from_netlist`
switches over; `compile()` does not. This stage exists to answer one question
before the expensive half is built: does relaxation place better than rows and
barycentres?

### Task 5: A linear solve with no dependency

**Files:**
- Create: `src/compile/relax/linear.rs`
- Create: `src/compile/relax/mod.rs` (declaring `mod linear;` only, for now)
- Modify: `src/compile/mod.rs` (add `pub mod relax;`)
- Test: `src/compile/relax/linear.rs`

**Interfaces:**
- Consumes: nothing. No imports outside `core`.
- Produces:
  - `pub struct Factorisation` with
    `Factorisation::of(matrix: &[f64], order: usize) -> Result<Factorisation, NotPositiveDefinite>`,
    `fn solve(&self, rhs: &mut [f64])`, `fn order(&self) -> usize`.
  - `pub struct NotPositiveDefinite { pub row: usize }`

- [ ] **Step 1: Write the failing tests**

Create `src/compile/relax/linear.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A system small enough to solve by hand:
    /// `4a + b = 1`, `a + 3b = 2` gives `a = 1/11`, `b = 7/11`.
    #[test]
    fn it_solves_a_system_whose_answer_is_known() {
        let factorisation =
            Factorisation::of(&[4.0, 1.0, 1.0, 3.0], 2).expect("this one is positive definite");
        let mut rhs = [1.0, 2.0];
        factorisation.solve(&mut rhs);
        assert!((rhs[0] - 1.0 / 11.0).abs() < 1e-12, "a came out {}", rhs[0]);
        assert!((rhs[1] - 7.0 / 11.0).abs() < 1e-12, "b came out {}", rhs[1]);
    }

    /// The Laplacian of one edge with nothing pinned. Translation is free, so
    /// there is no unique answer -- and a solver that returns one anyway
    /// returns a placement nobody can reproduce.
    #[test]
    fn a_system_with_no_unique_answer_is_refused() {
        let error = Factorisation::of(&[1.0, -1.0, -1.0, 1.0], 2)
            .expect_err("a free translation has no unique answer");
        assert_eq!(error.row, 1);
    }

    /// Pin one end of that same edge and it becomes solvable: the free body
    /// lands exactly on the pinned one, because a spring at rest has zero
    /// length.
    #[test]
    fn pinning_one_end_makes_the_same_system_solvable() {
        let factorisation = Factorisation::of(&[1.0], 1).expect("one pinned neighbour is enough");
        let mut rhs = [7.0];
        factorisation.solve(&mut rhs);
        assert!((rhs[0] - 7.0).abs() < 1e-12, "landed at {}", rhs[0]);
    }

    /// Same input, same bits. Everything downstream is reproducible only if
    /// this is.
    #[test]
    fn the_same_system_solves_to_the_same_bits_twice() {
        let matrix = [4.0, 1.0, 0.5, 1.0, 3.0, 0.25, 0.5, 0.25, 2.0];
        let first = {
            let mut rhs = [1.0, 2.0, 3.0];
            Factorisation::of(&matrix, 3).expect("positive definite").solve(&mut rhs);
            rhs
        };
        let second = {
            let mut rhs = [1.0, 2.0, 3.0];
            Factorisation::of(&matrix, 3).expect("positive definite").solve(&mut rhs);
            rhs
        };
        assert_eq!(first.map(f64::to_bits), second.map(f64::to_bits));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::linear`

Expected: compile failure -- module `relax` is not declared.

- [ ] **Step 3: Declare the modules**

`src/compile/mod.rs`, beside `pub mod physical;`:

```rust
pub mod relax;
```

`src/compile/relax/mod.rs`:

```rust
//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod linear;
```

- [ ] **Step 4: Write the implementation**

Prepend to `src/compile/relax/linear.rs`:

```rust
//! A dense Cholesky factorisation, and back-substitution against it.
//!
//! Deliberately small, and deliberately not a dependency. The crate has none
//! for linear algebra and compiles to wasm, where a foreign kernel's choice of
//! instruction is exactly the thing that would make native and browser layouts
//! disagree.
//!
//! Factorise once, solve many. The matrix relaxation builds is the spring
//! graph's weighted Laplacian with the pinned bodies struck out, and neither
//! the graph nor the stiffnesses change during a relaxation -- only the
//! right-hand side does, once per axis per step. A sparse solver would buy
//! nothing until circuits are much larger than seven_segment's couple of
//! hundred bodies, and would cost the property this one has for free: the loop
//! order is fixed, nothing is parallel, and `f64` addition, multiplication and
//! `sqrt` are exact IEEE-754 operations, so two toolchains agree bit for bit.

/// A symmetric positive-definite matrix, factorised as `L * Lᵀ`.
pub struct Factorisation {
    lower: Vec<f64>,
    order: usize,
}

/// Where the factorisation ran out of positive pivot.
///
/// For a Laplacian this means a connected component with nothing pinned in it:
/// the whole component may slide freely, so the system has no unique answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPositiveDefinite {
    pub row: usize,
}

impl Factorisation {
    /// Factorise `matrix`, given row-major as `order * order`. Only the lower
    /// triangle is read.
    pub fn of(matrix: &[f64], order: usize) -> Result<Factorisation, NotPositiveDefinite> {
        assert_eq!(matrix.len(), order * order, "matrix is not {order} by {order}");
        let mut lower = vec![0.0; order * order];
        for j in 0..order {
            let mut diagonal = matrix[j * order + j];
            for k in 0..j {
                diagonal -= lower[j * order + k] * lower[j * order + k];
            }
            if diagonal <= 0.0 {
                return Err(NotPositiveDefinite { row: j });
            }
            let pivot = diagonal.sqrt();
            lower[j * order + j] = pivot;
            for i in (j + 1)..order {
                let mut sum = matrix[i * order + j];
                for k in 0..j {
                    sum -= lower[i * order + k] * lower[j * order + k];
                }
                lower[i * order + j] = sum / pivot;
            }
        }
        Ok(Factorisation { lower, order })
    }

    /// Solve `matrix * x = rhs`, overwriting `rhs` with `x`.
    pub fn solve(&self, rhs: &mut [f64]) {
        assert_eq!(rhs.len(), self.order, "right-hand side is not {} long", self.order);
        for i in 0..self.order {
            let mut sum = rhs[i];
            for k in 0..i {
                sum -= self.lower[i * self.order + k] * rhs[k];
            }
            rhs[i] = sum / self.lower[i * self.order + i];
        }
        for i in (0..self.order).rev() {
            let mut sum = rhs[i];
            for k in (i + 1)..self.order {
                sum -= self.lower[k * self.order + i] * rhs[k];
            }
            rhs[i] = sum / self.lower[i * self.order + i];
        }
    }

    pub fn order(&self) -> usize {
        self.order
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --release --lib compile::relax::linear`

Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(relax): factorise once, solve every axis every step"
```

---

### Task 6: Bodies, pulls and welds from a netlist

The spring network is **not** the primitive graph. Two places they part, both
found by review and both load-bearing here:

- a **bare** merge contributes no primitive, and `expand` wires its consumers
  straight to its producers -- so springs alone would never notice the junction
  sitting between them. It is re-inserted as a body, and the pulls go through
  it.
- an **isolated** merge contributes one repeater per branch, and that repeater
  sits in the junction's socket because that is where the router terminates it.
  It is welded there rather than relaxed freely, which is what lets a gate with
  several bodies still hand back one anchor.

Building the pulls from the **netlist's declared inputs** rather than from
`graph.edges` gets the first of those for free: a gate's producer is looked up
by signal name, and a bare merge's own body is what that lookup returns.

**Files:**
- Create: `src/compile/relax/build.rs`
- Modify: `src/compile/relax/mod.rs` (declare `mod build;`, re-export the types)
- Test: `src/compile/relax/build.rs`

**Interfaces:**
- Consumes: `compile::geometry::CellFacing`, `compile::physical::{PortKind, RelativeSide, variants}`, `compile::primitive_graph::{PrimitiveGraph, NodeId, Provenance}`, `compile::topology::{Primitive, TemplateNode}`, `compile::{Netlist, Gate}`, `compile::planner::{Anchor, PortPlacements}`. Not `PlannerError`: `build` is below `planner` in the dependency order and must not reach up into it.
- Produces:
  - `pub struct Body { pub what: BodyKind, pub position: [f64; 3], pub facing: CellFacing, pub pinned: bool }`
  - `pub enum BodyKind { Primitive { node: NodeId, kind: Primitive }, Junction { gate: usize } }`
  - `pub enum Attach { Socket(usize), Pin, Port(PortKind) }`
  - `pub struct Pull { pub from: (usize, Attach), pub to: (usize, Attach), pub stiffness: f64 }`
  - `pub enum Weld { AtSocket { repeater: usize, junction: usize, input_index: usize }, BesideAt { lock: usize, data: usize, side: RelativeSide } }`
  - `pub struct BodyGraph { pub bodies: Vec<Body>, pub pulls: Vec<Pull>, pub welds: Vec<Weld>, pub nodes: Vec<Vec<usize>>, pub anchor_body: Vec<usize> }`
  - `pub fn attach_offset(attach: Attach, body: &Body) -> [f64; 3]`
  - `pub struct Cell { pub offset: (i32, i32, i32), pub carries: Option<String> }`
  - `pub fn cells(body: &Body) -> Vec<Cell>`
  - `pub fn build(netlist: &Netlist, graph: &PrimitiveGraph, start: &[Anchor], pinned: &PortPlacements) -> Result<BodyGraph, String>` -- the error is a sentence, because the only two failures are "a gate instantiated no primitive" and "a declared input has no lever", and `RelaxError::CannotBuild` is what carries it
  - `pub const SIGNAL_STIFFNESS: f64 = 1.0;`

- [ ] **Step 1: Write the failing tests**

Create `src/compile/relax/build.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::library::Library;
    use crate::compile::primitive_graph::expand;
    use crate::compile::{Gate, Netlist};

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    fn built(netlist: &Netlist) -> BodyGraph {
        let graph = expand(netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; netlist.gates.len() + netlist.inputs.len()];
        build(netlist, &graph, &start, &PortPlacements::default()).expect("builds")
    }

    /// A bare merge places nothing, so the primitive graph wires its consumer
    /// straight to its producer. The spring network must not: the junction is
    /// a real cell in a real place, and springs that skip it place the two
    /// sides on top of it.
    #[test]
    fn a_bare_merge_gets_a_body_and_the_pulls_go_through_it() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![
                nor("na", &["a"]),
                nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                nor("out", &["m"]),
            ],
        };
        let graph = built(&netlist);

        let junction = graph
            .bodies
            .iter()
            .position(|body| matches!(body.what, BodyKind::Junction { gate: 2 }))
            .expect("the merge has a junction body");

        assert!(
            graph.pulls.iter().any(|pull| pull.from.0 == junction),
            "nothing leaves the junction, so its consumer was wired past it"
        );
        assert_eq!(
            graph.pulls.iter().filter(|pull| pull.to.0 == junction).count(),
            2,
            "both branches must arrive at the junction"
        );
    }

    /// An isolated branch's repeater is a free body everywhere except where it
    /// actually goes. `world_partition::resolve_node_position` already says it
    /// is in the junction's socket; a weld is that statement, made a
    /// constraint.
    #[test]
    fn an_isolated_branch_welds_its_repeater_into_the_junctions_socket() {
        // `nb` feeds both the merge and `spy`, so the merge's branch on it is
        // shared rather than bare, and `expand` gives it a repeater.
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into(), "spy".into()],
            gates: vec![
                nor("na", &["a"]),
                nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                nor("out", &["m"]),
                nor("spy", &["nb"]),
            ],
        };
        let graph = built(&netlist);

        let junction = graph
            .bodies
            .iter()
            .position(|body| matches!(body.what, BodyKind::Junction { gate: 2 }))
            .expect("the merge has a junction body");
        let weld = graph
            .welds
            .iter()
            .find(|weld| matches!(weld, Weld::AtSocket { junction: j, .. } if *j == junction))
            .expect("the isolated branch is welded");
        let Weld::AtSocket { repeater, input_index, .. } = *weld else {
            unreachable!("matched AtSocket above")
        };

        assert_eq!(input_index, 1, "`nb` is the merge's second declared input");
        assert!(
            matches!(
                graph.bodies[repeater].what,
                BodyKind::Primitive { kind: Primitive::Repeater, .. }
            ),
            "a weld must hold a repeater"
        );
    }

    /// A declared output's lamp hangs under its producer's pin -- that is what
    /// `emit_primitives` does and `PlanCandidate` has no anchor for it. A body
    /// with no position to choose is not a body.
    #[test]
    fn a_declared_outputs_lamp_is_not_a_body() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);

        assert!(
            !graph.bodies.iter().any(|body| matches!(
                body.what,
                BodyKind::Primitive { kind: Primitive::Lamp, .. }
            )),
            "a lamp's position is its producer's, not its own"
        );
        assert_eq!(
            graph.bodies.len(),
            2,
            "one torch and one lever, and nothing else"
        );
    }

    /// Every node `PlanCandidate` expects has a body to be the anchor for --
    /// gates first, then primary inputs, which is the order `emit_primitives`
    /// reads positionally.
    #[test]
    fn every_candidate_node_has_an_anchor_body() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("na", &["a"]), nor("out", &["na", "b"])],
        };
        let graph = built(&netlist);

        assert_eq!(graph.anchor_body.len(), netlist.gates.len() + netlist.inputs.len());
        assert_eq!(graph.nodes.len(), graph.anchor_body.len());
        for (node, &body) in graph.anchor_body.iter().enumerate() {
            assert!(
                graph.nodes[node].contains(&body),
                "node {node}'s anchor body is not one of its own bodies"
            );
        }
    }

    /// A pinned port takes no force. Recorded here rather than discovered in
    /// the solve, because the solve's matrix is built by striking pinned
    /// bodies out of it.
    #[test]
    fn a_pinned_port_is_a_pinned_body() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let mut placements = PortPlacements::default();
        placements.pin("a", Anchor { x: 40, y: 1, z: 9 });

        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 2];
        let built = build(&netlist, &graph, &start, &placements).expect("builds");

        let lever = built.anchor_body[1];
        assert!(built.bodies[lever].pinned, "a pinned input must be a pinned body");
        assert_eq!(built.bodies[lever].position, [40.0, 1.0, 9.0]);
        assert!(!built.bodies[built.anchor_body[0]].pinned, "nothing pinned the gate");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::build`

Expected: compile failure -- `BodyGraph`, `build`, `Weld` do not exist.

- [ ] **Step 3: Write the types**

Prepend to `src/compile/relax/build.rs`:

```rust
//! Turning a netlist and its primitive graph into the thing that relaxes.
//!
//! Placement's graph is not quite the primitive graph, and this module is
//! where they part. Two differences, both about wire merges:
//!
//! - a **bare** merge contributes no primitive, and `expand` splices its
//!   consumers straight onto its producers. Springs alone would pull those two
//!   groups together and never notice the junction between them, so the
//!   junction is added as a body and the pulls are routed through it.
//! - an **isolated** merge contributes one repeater per branch, and that
//!   repeater's cell is the junction's socket for that branch -- which is what
//!   `world_partition::resolve_node_position` already answers. It is welded
//!   there, not relaxed freely.
//!
//! Pulls are built from the netlist's declared inputs rather than from
//! `graph.edges`, which gets the first of those for nothing: looking a
//! producer up by signal name returns a bare merge's own junction, where
//! walking edges would have skipped it.

use crate::compile::geometry::{self, CellFacing};
use crate::compile::physical::{self, PortKind, RelativeSide};
use crate::compile::planner::{Anchor, PortPlacements};
use crate::compile::primitive_graph::{NodeId, PrimitiveGraph, Provenance};
use crate::compile::topology::{Primitive, TemplateNode};
use crate::compile::Netlist;

/// Every signal spring pulls the same.
///
/// The spec defers per-edge weighting to the criticality question, and a
/// stiffness that varies without a measurement behind it is the sort of number
/// this project has already spent time removing from the planner.
///
/// The one exception the spec names -- cell cohesion, at the graph's maximum
/// degree -- has no caller in this stage. Every gate with more than one body
/// today is a merge, and a merge holds itself together with a [`Weld`] rather
/// than with a stiff spring. Cohesion arrives with Design H, which is the
/// first gate whose members are genuinely free to move apart.
pub const SIGNAL_STIFFNESS: f64 = 1.0;

/// One thing relaxation may move.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub what: BodyKind,
    pub position: [f64; 3],
    /// The net each declared input arrives on, in declared order.
    ///
    /// Copied from the netlist at build time rather than looked up later,
    /// because a facing changes where a body's cells are and never changes
    /// what they carry -- so the labels are settled once and the offsets are
    /// recomputed every round.
    pub inputs: Vec<String>,
    /// The net this body drives, if it drives one. `None` for a body that is
    /// not the one carrying its gate's output -- an isolated branch's
    /// repeater drives into the junction, not out of the gate.
    pub output: Option<String>,
    /// One of four. Never continuous: a body's best facing is found by trying
    /// all four against the pulls on its ports, so there is no angle to
    /// integrate and none to quantise later.
    pub facing: CellFacing,
    /// Fixed by `PortPlacements`. A pinned body contributes force to its
    /// neighbours and takes none, and `snap` returns it where it was pinned.
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// A component the primitive graph named. Its blocks include whatever it
    /// stands on or attaches to; `physical.rs` has always said so.
    Primitive { node: NodeId, kind: Primitive },
    /// A declared wire merge. `expand` produces no primitive for one, and
    /// `place_merge_gate` writes blocks at its anchor regardless.
    Junction { gate: usize },
}

/// Where on a body a spring attaches.
///
/// `Socket` and `Pin` are gate-cell geometry, shared by a NOR and a merge
/// because `place_merge_gate` is built to a NOR's exact footprint. `Port` is
/// for the primitives whose endpoints `physical.rs` names, which is how an
/// isolated branch's repeater says it reads at its rear.
///
/// A NOR's three declared inputs are three *sockets*, not three uses of one
/// `TorchInput` port: they arrive on three different faces, and collapsing
/// them onto the support's one port would tell the solver they are the same
/// place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// The cell a gate's `index`-th declared input arrives in. Air until the
    /// router fills it, and part of the gate either way -- `gate_footprint`
    /// counts it a conductor, because what ends up there is dust or a
    /// repeater.
    Socket(usize),
    /// The cell a gate's outgoing net starts from: one hop out from its torch
    /// for a NOR, one hop out from its junction for a merge.
    Pin,
    /// A port `physical.rs` names, for a body placed as a primitive rather
    /// than as a gate cell.
    Port(PortKind),
}

/// A spring, attached at a point on each end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pull {
    pub from: (usize, Attach),
    pub to: (usize, Attach),
    pub stiffness: f64,
}

/// A relation between two bodies that must hold exactly. Projected, never
/// pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weld {
    /// An isolated merge branch's repeater, in the junction's socket for that
    /// branch.
    AtSocket {
        repeater: usize,
        junction: usize,
        input_index: usize,
    },
    /// Design H's lock repeater at the data repeater's side. No caller yet:
    /// `compile()` rejects `GateKind::DffPosedge` before placement, so there
    /// is no Design H region to place.
    BesideAt {
        lock: usize,
        data: usize,
        side: RelativeSide,
    },
}

/// Everything relaxation moves, and everything that decides where.
#[derive(Debug, Clone)]
pub struct BodyGraph {
    pub bodies: Vec<Body>,
    pub pulls: Vec<Pull>,
    pub welds: Vec<Weld>,
    /// Which bodies belong to each of `PlanCandidate`'s nodes -- gates first,
    /// then primary inputs, which is the positional order `emit_primitives`
    /// reads. This is what `snap` collapses through.
    pub nodes: Vec<Vec<usize>>,
    /// Which body carries each node's anchor: a NOR's torch, a merge's
    /// junction, an input's lever.
    pub anchor_body: Vec<usize>,
}
```

- [ ] **Step 4: Write `attach_offset`**

```rust
/// Where an attachment sits relative to its body's own position.
///
/// A gate cell's origin is its support (a NOR) or its junction (a merge), and
/// both put their sockets on `geometry::input_directions` and their pin out
/// along `geometry::output_direction` -- a NOR two hops, because its torch
/// stands in the first one, a merge one.
pub fn attach_offset(attach: Attach, body: &Body) -> [f64; 3] {
    let facing = body.facing;
    match attach {
        Attach::Socket(index) => {
            let direction = geometry::input_directions(facing)[index];
            let step = crate::redstone::simulator::position::Position::new(0, 0, 0).offset(direction);
            [step.x as f64, step.y as f64, step.z as f64]
        }
        Attach::Pin => {
            let hops = match body.what {
                BodyKind::Junction { .. } => 1,
                BodyKind::Primitive { .. } => 2,
            };
            let direction = geometry::output_direction(facing);
            let mut step = crate::redstone::simulator::position::Position::new(0, 0, 0);
            for _ in 0..hops {
                step = step.offset(direction);
            }
            [step.x as f64, step.y as f64, step.z as f64]
        }
        Attach::Port(kind) => {
            let BodyKind::Primitive { kind: primitive, .. } = body.what else {
                unreachable!("a junction has no `physical` port; use Socket or Pin")
            };
            let port = physical::variants(primitive)[usize::from(facing.index())].port(kind);
            [
                port.position.x as f64,
                port.position.y as f64,
                port.position.z as f64,
            ]
        }
    }
}
```

`Attach::Socket(index)` panics for `index >= 3`, which is correct: a gate with
four declared inputs is one `place_nor_gate` already refuses.

- [ ] **Step 5: Write `cells` -- what a body occupies, and what each cell carries**

Separation is between **cells carrying different signals**, not between body
centres. A body is not a point: a torch is its support and its torch block, and
which of those two a foreign net may run against is the difference between a
legal circuit and a legal circuit that computes the wrong function. On
2026-08-12 the planner placed a full adder that passed all four invariants and
computed the wrong sum, because a foreign net was free to run against a NOR's
support -- which is the gate's input node -- that the code treated as inert.

```rust
/// One cell a body occupies, and the signal it carries.
///
/// `None` is inert: solid material a foreign net may run beside. A repeater's
/// `DOWN` floor is the case that exists today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub offset: (i32, i32, i32),
    pub carries: Option<String>,
}

/// Every cell `body` occupies, and what each carries.
///
/// A gate cell's answer is `place_nor_gate`'s and `place_merge_gate`'s, stated
/// as signals rather than as blocks:
///
/// - the **support** (or junction) carries every declared input's net. It is
///   the gate's input node -- dust laid against it powers it -- and a NOR is
///   N sources into one sink, so it is on all of them at once.
/// - each **socket** carries its own branch's net, and is a conductor even
///   though it is air: what ends up there is dust or a repeater.
/// - the **torch** and the **pin** carry the gate's own output net.
/// - a **repeater's floor** carries nothing.
pub fn cells(body: &Body) -> Vec<Cell> {
    let facing = body.facing;
    let mut cells = Vec::new();

    match (&body.output, body.inputs.is_empty()) {
        // A body carrying a gate's output is a gate cell: support or junction,
        // sockets, torch, pin.
        (Some(output), _) => {
            // The support or junction is the gate's input node -- N sources
            // into one sink -- so it is on every input net at once. Modelled
            // as the first, which is enough: what matters is that it is *not*
            // inert and *not* on the output net.
            cells.push(Cell {
                offset: (0, 0, 0),
                carries: body.inputs.first().cloned(),
            });
            for (index, signal) in body.inputs.iter().enumerate() {
                let step = Position::new(0, 0, 0).offset(geometry::input_directions(facing)[index]);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: Some(signal.clone()),
                });
            }

            let out = geometry::output_direction(facing);
            // A NOR's torch stands between origin and pin; a merge's junction
            // is the origin, so its pin is one hop rather than two.
            let hops = match body.what {
                BodyKind::Junction { .. } => 1,
                BodyKind::Primitive { .. } => 2,
            };
            let mut step = Position::new(0, 0, 0);
            for _ in 0..hops {
                step = step.offset(out);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: Some(output.clone()),
                });
            }
            if matches!(body.what, BodyKind::Junction { .. }) {
                // `place_merge_gate` floors its own junction; a NOR does not,
                // because a support block needs nothing beneath it.
                cells.push(Cell { offset: (0, -1, 0), carries: None });
            }
        }
        // Anything else is placed as a primitive: an isolated branch's
        // repeater, or a primary input's lever. `physical.rs` says which cells
        // it occupies, and a variant's blocks already include what it stands
        // on.
        (None, _) => {
            let BodyKind::Primitive { kind, .. } = body.what else {
                unreachable!("a junction always carries its gate's output")
            };
            let variant = &physical::variants(kind)[usize::from(facing.index())];
            for block in variant.blocks {
                cells.push(Cell {
                    offset: (block.position.x, block.position.y, block.position.z),
                    carries: match block.kind {
                        // A repeater's floor and a lever's floor are inert: a
                        // net may run beside them.
                        BlockKind::Solid => None,
                        _ => body.inputs.first().cloned(),
                    },
                });
            }
        }
    }
    cells
}
```

A lever is the one primitive body with an `output` and no `inputs`, so it takes
the gate-cell arm with an empty socket list: origin, then its pin one hop out.
That is exactly what `place_primary_input` writes.

**Two exemptions the projection needs, both from this table.** Cells sharing a
signal are exempt: a producer's pin and its consumer's socket carry the same
net, and the route between them is what makes them one. Cells with `carries:
None` are exempt from everything except occupying the same cell as another
body.

- [ ] **Step 6: Refuse a primitive with no variants**

`variants(Primitive::Comparator)` returns an empty slice, and `PortKind`
declares `ComparatorRear`, `ComparatorSide` and `ComparatorFront` that nothing
constructs. No library entry uses a comparator today, so relaxation never meets
one -- but that is an accident of the current library rather than a rule, and
indexing an empty slice would panic somewhere unhelpful.

The guard goes in `build`, beside every primitive body it creates -- written
out in Step 7. A body whose primitive has no variants is an error naming the
primitive, not a silent placement of nothing, and not a panic from indexing an
empty slice somewhere with no context.

Add the test:

```rust
    /// A primitive `physical.rs` has no variants for cannot be placed, and
    /// says so by name.
    #[test]
    fn a_primitive_with_no_variants_is_refused_by_name() {
        assert!(
            physical::variants(Primitive::Comparator).is_empty(),
            "this test is about the primitive that has none; if that changed, \
             pick another or delete this"
        );
    }
```

which is a guard rather than a placement: no netlist can reach a comparator
today, so the reachable half of the claim is the `is_empty` check in `build`.

- [ ] **Step 7: Write `build`**

```rust
/// Bodies, pulls and welds for `netlist`, started from `start`.
///
/// `start` is one anchor per `PlanCandidate` node -- gates, then primary
/// inputs -- which is what `plan_from_netlist`'s existing depth-and-barycentre
/// layout produces. Relaxation improves a known-bad answer rather than
/// inventing one, and the improvement is measurable against the numbers it
/// started from.
pub fn build(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
) -> Result<BodyGraph, String> {
    let node_count = netlist.gates.len() + netlist.inputs.len();
    assert_eq!(start.len(), node_count, "one start anchor per candidate node");

    let mut bodies: Vec<Body> = Vec::new();
    let mut nodes: Vec<Vec<usize>> = vec![Vec::new(); node_count];
    let mut anchor_body: Vec<usize> = vec![usize::MAX; node_count];
    let mut welds: Vec<Weld> = Vec::new();

    for (gate_index, gate) in netlist.gates.iter().enumerate() {
        let at = start[gate_index];
        let position = [at.x as f64, at.y as f64, at.z as f64];
        let is_pinned = pinned.get(&gate.output).is_some();

        // The body that carries this gate's anchor: a merge's junction, or the
        // single torch its library entry instantiated.
        let anchor = if gate.is_merge() {
            bodies.push(Body {
                what: BodyKind::Junction { gate: gate_index },
                position,
                inputs: gate.inputs.clone(),
                output: Some(gate.output.clone()),
                facing: CellFacing::NORTH,
                pinned: is_pinned,
            });
            bodies.len() - 1
        } else {
            let node = *graph.gate_nodes[gate_index].first().ok_or_else(|| {
                format!("gate `{}` instantiated no primitive", gate.output)
            })?;
            let kind = graph.nodes[node].primitive;
            if physical::variants(kind).is_empty() {
                return Err(format!(
                    "gate `{}` needs a `{kind:?}`, which has no physical variants",
                    gate.output
                ));
            }
            bodies.push(Body {
                what: BodyKind::Primitive { node, kind },
                position,
                inputs: gate.inputs.clone(),
                output: Some(gate.output.clone()),
                facing: CellFacing::NORTH,
                pinned: is_pinned,
            });
            bodies.len() - 1
        };
        nodes[gate_index].push(anchor);
        anchor_body[gate_index] = anchor;

        // An isolated merge's branch repeaters, welded into the sockets the
        // router terminates them in.
        if gate.is_merge() {
            for &node in &graph.gate_nodes[gate_index] {
                let Provenance::Gate {
                    role: TemplateNode::IsolatingRepeater(input_index),
                    ..
                } = graph.nodes[node].provenance
                else {
                    continue;
                };
                let direction = geometry::input_directions(CellFacing::NORTH)[input_index];
                let socket = crate::redstone::simulator::position::Position::new(at.x, at.y, at.z)
                    .offset(direction);
                bodies.push(Body {
                    what: BodyKind::Primitive {
                        node,
                        kind: graph.nodes[node].primitive,
                    },
                    position: [socket.x as f64, socket.y as f64, socket.z as f64],
                    // Its branch's net, and no output of its own: it drives
                    // into the junction, not out of the gate.
                    inputs: vec![gate.inputs[input_index].clone()],
                    output: None,
                    facing: CellFacing::NORTH,
                    pinned: is_pinned,
                });
                let repeater = bodies.len() - 1;
                nodes[gate_index].push(repeater);
                welds.push(Weld::AtSocket {
                    repeater,
                    junction: anchor,
                    input_index,
                });
            }
        }
    }

    for (input_index, name) in netlist.inputs.iter().enumerate() {
        let node = graph.nodes.len();
        let node = (0..node)
            .find(|&candidate| {
                matches!(&graph.nodes[candidate].provenance,
                    Provenance::PrimaryInput { name: declared } if declared == name)
            })
            .ok_or_else(|| format!("declared input `{name}` has no lever"))?;
        let candidate_node = netlist.gates.len() + input_index;
        let at = start[candidate_node];
        bodies.push(Body {
            what: BodyKind::Primitive {
                node,
                kind: graph.nodes[node].primitive,
            },
            position: [at.x as f64, at.y as f64, at.z as f64],
            // A lever drives its own name and reads nothing, which is what
            // gives it a gate cell's shape with no sockets.
            inputs: Vec::new(),
            output: Some(name.clone()),
            facing: CellFacing::NORTH,
            pinned: pinned.get(name).is_some(),
        });
        nodes[candidate_node].push(bodies.len() - 1);
        anchor_body[candidate_node] = bodies.len() - 1;
    }

    let pulls = signal_pulls(netlist, &bodies, &nodes, &anchor_body, &welds);

    Ok(BodyGraph {
        bodies,
        pulls,
        welds,
        nodes,
        anchor_body,
    })
}

/// One pull per declared gate input: from the producer's outgoing pin to the
/// consumer's socket for that branch.
///
/// A declared output's lamp gets none. `emit_primitives` hangs it under its
/// producer's pin and `PlanCandidate` has no anchor for it, so its position is
/// not something relaxation chooses.
fn signal_pulls(
    netlist: &Netlist,
    bodies: &[Body],
    nodes: &[Vec<usize>],
    anchor_body: &[usize],
    welds: &[Weld],
) -> Vec<Pull> {
    let mut producer_node = std::collections::BTreeMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        producer_node.insert(gate.output.as_str(), index);
    }
    for (index, name) in netlist.inputs.iter().enumerate() {
        producer_node.insert(name.as_str(), netlist.gates.len() + index);
    }

    let mut pulls = Vec::new();
    for (gate_index, gate) in netlist.gates.iter().enumerate() {
        for (input_index, signal) in gate.inputs.iter().enumerate() {
            let Some(&producer) = producer_node.get(signal.as_str()) else {
                continue;
            };
            let from = (anchor_body[producer], Attach::Pin);

            // A branch with a welded repeater arrives at the repeater's rear;
            // the weld, not a spring, is what puts the repeater in the socket.
            let welded = welds.iter().find_map(|weld| match weld {
                Weld::AtSocket {
                    repeater,
                    junction,
                    input_index: branch,
                } if *junction == anchor_body[gate_index] && *branch == input_index => {
                    Some(*repeater)
                }
                _ => None,
            });
            let to = match welded {
                Some(repeater) => (repeater, Attach::Port(PortKind::RepeaterRear)),
                None => (anchor_body[gate_index], Attach::Socket(input_index)),
            };

            pulls.push(Pull {
                from,
                to,
                stiffness: SIGNAL_STIFFNESS,
            });
        }
    }
    let _ = (bodies, nodes);
    pulls
}
```

- [ ] **Step 8: Declare the module and re-export**

`src/compile/relax/mod.rs`:

```rust
mod build;
mod linear;

pub use build::{attach_offset, Attach, Body, BodyGraph, BodyKind, Pull, Weld, SIGNAL_STIFFNESS};
```

- [ ] **Step 9: Run the tests**

Run: `cargo test --release --lib compile::relax::build`

Expected: 6 passed.

- [ ] **Step 10: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. Nothing calls `build` outside its own tests yet.

- [ ] **Step 11: Commit**

```bash
git add -A && git commit -m "feat(relax): the spring network is not the primitive graph, and here is where they part"
```

---

### Task 7: The separation rule, and the welds that outrank it

Springs pull and separation pushes, so the relaxed solution sits at exactly the
minimum separation everywhere. Four terms decide what that minimum is, and each
has a source.

**Files:**
- Create: `src/compile/relax/project.rs`
- Modify: `src/compile/relax/mod.rs`
- Test: `src/compile/relax/project.rs`

**Interfaces:**
- Consumes: `build::{Body, BodyKind, BodyGraph, Cell, Weld, cells}`, `compile::geometry`.
- Produces:
  - `pub const CONDUCTOR_CLEARANCE: f64 = 2.0;`
  - `pub const SNAP_MARGIN: f64 = 1.0;`
  - `pub const ROUTE_PITCH: f64 = 2.0;`
  - `pub const PROJECTION_ROUNDS: usize = 64;`
  - `pub fn reservation(routed_degree: usize) -> f64`
  - `pub struct Violation { pub left: usize, pub right: usize, pub shortfall: f64 }`
  - `pub struct Axes(&'static [usize]);` with `Axes::IN_PLANE` (`&[0, 2]`) and `Axes::ALL` (`&[0, 1, 2]`)
  - `pub fn project(graph: &mut BodyGraph, required: &[f64], axes: Axes) -> Result<(), Violation>`
  - `pub fn worst_violation(graph: &BodyGraph, required: &[f64]) -> Option<Violation>`
  - `pub fn required_separations(graph: &BodyGraph) -> Vec<f64>`
  - `pub struct PlacedCell { pub at: [f64; 3], pub carries: Option<String> }`
  - `pub fn placed_cells(graph: &BodyGraph) -> Vec<Vec<PlacedCell>>`

- [ ] **Step 1: Write the failing tests**

Create `src/compile/relax/project.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::geometry::CellFacing;
    use crate::compile::relax::build::{Body, BodyGraph, BodyKind, Weld};
    use crate::compile::physical::RelativeSide;
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

    /// Stage 1 may not spend height. Bodies stay at the Y their starting
    /// layout gave them, so a projection that reaches for the third dimension
    /// here has changed what the stage promised.
    #[test]
    fn in_plane_projection_never_moves_a_body_in_y() {
        let mut graph = graph_of(
            vec![body(0.0, 1.0, 0.0), body(0.1, 1.0, 0.1), body(0.2, 1.0, 0.2)],
            Vec::new(),
        );
        let required = vec![3.0; 3];
        let _ = project(&mut graph, &required, Axes::IN_PLANE);
        for (index, body) in graph.bodies.iter().enumerate() {
            assert_eq!(body.position[1], 1.0, "body {index} left its storey");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::project`

Expected: compile failure -- `project`, `Axes`, `reservation` do not exist.

- [ ] **Step 3: Write the constants and the reservation**

Prepend to `src/compile/relax/project.rs`:

```rust
//! The hard constraints: how far apart two bodies must be, and what has to
//! hold exactly.
//!
//! Separation is projected rather than added as a force, because the number it
//! enforces is derived rather than tuned -- and because a force that competes
//! with the springs settles at whatever the two balance out to, which is a
//! layout that is nearly legal.

use crate::compile::geometry;
use crate::compile::relax::build::{Body, BodyGraph, BodyKind, Weld};
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
pub const PROJECTION_ROUNDS: usize = 64;

/// Room a body reserves beyond its own clearance for the routes that must
/// reach it.
///
/// Routes arrive from every side, so `d` lanes at [`ROUTE_PITCH`] sit on a
/// ring rather than in a line: a ring at radius `r` around a cell has about
/// `8r` lattice cells on it, and `8r >= ROUTE_PITCH * d` gives `r >= d / 4`.
///
/// **This is the design's one guessed number.** The spec says how it fails: a
/// halo is not a channel, and a high-degree gate gets a large ring whether or
/// not its neighbours needed one. If placements come out routable but
/// wasteful, or compact but unroutable, this is what was wrong.
pub fn reservation(routed_degree: usize) -> f64 {
    routed_degree as f64 / 4.0
}
```

- [ ] **Step 4: Write the separation predicate and the required table**

```rust
/// Which axes a repair may use.
///
/// Stage 1 is in-plane: bodies stay at the Y their starting layout gave them.
/// Stage 2 adds Y, and that one-word difference is the whole of "let
/// separation choose the axis".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axes(&'static [usize]);

impl Axes {
    pub const IN_PLANE: Axes = Axes(&[0, 2]);
    pub const ALL: Axes = Axes(&[0, 1, 2]);

    fn iter(self) -> impl Iterator<Item = usize> {
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
    let mut degree = vec![0usize; graph.bodies.len()];
    for pull in &graph.pulls {
        degree[pull.from.0] += 1;
        degree[pull.to.0] += 1;
    }
    // A welded pair is adjacent by construction, so no wire runs between them
    // and neither reserves room for one.
    for weld in &graph.welds {
        let (left, right) = match *weld {
            Weld::AtSocket { repeater, junction, .. } => (repeater, junction),
            Weld::BesideAt { lock, data, .. } => (lock, data),
        };
        degree[left] = degree[left].saturating_sub(1);
        degree[right] = degree[right].saturating_sub(1);
    }
    degree
        .into_iter()
        .map(|routed| CONDUCTOR_CLEARANCE + reservation(routed) + SNAP_MARGIN)
        .collect()
}

/// How far short of the requirement the closest offending pair of cells is.
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
/// Conservative in two ways the derivation would allow relaxing -- it forbids
/// the horizontal diagonal, which `dust_reach` has no case for, and it ignores
/// that a repeater is a firewall on its non-facing sides. Both are a
/// measurement away, and both are the first thing to try if layouts come out
/// sparse.
fn shortfall(left: &[PlacedCell], right: &[PlacedCell], required: f64) -> f64 {
    let mut worst: f64 = 0.0;
    for here in left {
        for there in right {
            // Same net: the route between them is what makes them one thing.
            if here.carries.is_some() && here.carries == there.carries {
                continue;
            }
            // Inert material -- a repeater's floor -- keeps nothing out but
            // its own cell, which cell exclusivity already covers.
            if here.carries.is_none() || there.carries.is_none() {
                continue;
            }
            let dx = (here.at[0] - there.at[0]).abs();
            let dy = (here.at[1] - there.at[1]).abs();
            let dz = (here.at[2] - there.at[2]).abs();
            if dy >= CONDUCTOR_CLEARANCE {
                continue;
            }
            worst = worst.max(required - dx.max(dz));
        }
    }
    worst.max(0.0)
}

/// One of a body's cells, in world coordinates.
///
/// Recomputed each round rather than cached: a body that turned has moved
/// every cell it owns, and a cached one would be the layout before the turn.
#[derive(Debug, Clone)]
pub struct PlacedCell {
    pub at: [f64; 3],
    pub carries: Option<String>,
}

/// Where every body's cells are right now.
pub fn placed_cells(graph: &BodyGraph) -> Vec<Vec<PlacedCell>> {
    graph
        .bodies
        .iter()
        .map(|body| {
            crate::compile::relax::build::cells(body)
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
```

- [ ] **Step 5: Write the exemption**

```rust
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
fn exempt(graph: &BodyGraph, left: usize, right: usize) -> bool {
    graph.welds.iter().any(|weld| {
        let pair = match *weld {
            Weld::AtSocket { repeater, junction, .. } => (repeater, junction),
            Weld::BesideAt { lock, data, .. } => (lock, data),
        };
        pair == (left, right) || pair == (right, left)
    })
}

/// The worst pair still too close, for an error that names something.
pub fn worst_violation(graph: &BodyGraph, required: &[f64]) -> Option<Violation> {
    let cells = placed_cells(graph);
    let mut worst: Option<Violation> = None;
    for left in 0..graph.bodies.len() {
        for right in (left + 1)..graph.bodies.len() {
            if exempt(graph, left, right) {
                continue;
            }
            let need = required[left].max(required[right]);
            let short = shortfall(&cells[left], &cells[right], need);
            if short > 0.0 && worst.is_none_or(|current| short > current.shortfall) {
                worst = Some(Violation { left, right, shortfall: short });
            }
        }
    }
    worst
}
```

The pair's requirement is `max`, not the sum: a reservation is a ring around
one body, and two rings that overlap are still one corridor. Taking the sum
would charge a low-degree neighbour for its neighbour's fan-out.

- [ ] **Step 6: Write the projection**

```rust
/// Separate every violating pair, then re-satisfy every weld, and repeat until
/// neither moves anything.
///
/// Welds last, deliberately: if only one can hold at the end of a round it
/// must be the one whose failure the invariants would not catch as a wrong
/// answer.
pub fn project(graph: &mut BodyGraph, required: &[f64], axes: Axes) -> Result<(), Violation> {
    for _ in 0..PROJECTION_ROUNDS {
        let mut moved = false;
        // Recomputed once per round, not once per pair: within a round the
        // separations are decided against one consistent picture, which is
        // what keeps the order of the pair loop from changing the answer.
        let cells = placed_cells(graph);
        for left in 0..graph.bodies.len() {
            for right in (left + 1)..graph.bodies.len() {
                if exempt(graph, left, right) {
                    continue;
                }
                let need = required[left].max(required[right]);
                let short = shortfall(&cells[left], &cells[right], need);
                if short <= 0.0 {
                    continue;
                }
                separate(graph, left, right, short, axes);
                moved = true;
            }
        }
        let welds = graph.welds.clone();
        for weld in &welds {
            moved |= satisfy(graph, weld);
        }
        if !moved {
            return Ok(());
        }
    }
    match worst_violation(graph, required) {
        Some(violation) => Err(violation),
        None => Ok(()),
    }
}

/// Push one pair apart along whichever allowed axis costs least.
///
/// `short` is what [`shortfall`] measured on the closest offending pair of
/// cells, so moving the bodies that far along any axis clears it. Y is charged
/// [`CONDUCTOR_CLEARANCE`] flat instead, for the reason [`shortfall`] gives:
/// height does not carry the routing reservation, so stacking is cheaper than
/// spreading exactly when a region is crowded.
fn separate(graph: &mut BodyGraph, left: usize, right: usize, short: f64, axes: Axes) {
    let (mut axis, mut cost) = (usize::MAX, f64::INFINITY);
    for candidate in axes.iter() {
        let deficit = if candidate == 1 {
            let gap = (graph.bodies[left].position[1] - graph.bodies[right].position[1]).abs();
            CONDUCTOR_CLEARANCE - gap
        } else {
            short
        };
        if deficit < cost {
            axis = candidate;
            cost = deficit;
        }
    }
    if cost <= 0.0 {
        return;
    }

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
            let port = crate::compile::physical::variants(kind)
                [usize::from(facing.index())]
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
```

- [ ] **Step 7: Declare the module**

`src/compile/relax/mod.rs` gains `mod project;` and re-exports
`project::{reservation, Axes, Violation, CONDUCTOR_CLEARANCE, SNAP_MARGIN}`.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::relax::project`

Expected: 6 passed.

- [ ] **Step 9: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`.

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(relax): separation pushes, welds win, and a deadlock says so"
```

---

### Task 8: A step is a solve, a choice among four, and a projection

A step is not a gradient step. The objective is quadratic, so with the
constraints held aside it has a direct solution -- the `Ax = f + c` the
founding spec records LogicLoom solving -- and a step is: solve it exactly for
the current facings, choose each body's best facing for the current positions,
project. Repeat.

Holding facings aside is what makes it linear. Choosing them is a
one-dimensional question with four answers, so it is an enumeration rather than
a rotation integrated over time.

**Files:**
- Modify: `src/compile/relax/mod.rs`
- Test: `src/compile/relax/mod.rs`

**Interfaces:**
- Consumes: `linear::Factorisation`, `build::{BodyGraph, attach_offset}`, `project::{project, required_separations, worst_violation, Axes, Violation}`.
- Produces:
  - `pub struct RelaxEffort { pub iterations: usize, pub seed: u64 }` with `Default` (`iterations: 256, seed: 0`)
  - `pub struct ContinuousPlacement { pub graph: BodyGraph, pub converged: bool, pub iterations: usize }`
  - `pub enum RelaxError { DidNotConverge { iterations: usize, worst: Violation }, Deadlocked { worst: Violation }, Unsolvable { component_row: usize } }` with `Display`
  - `pub const CONVERGED: f64 = 0.1;`
  - `pub fn relax(netlist: &Netlist, graph: &PrimitiveGraph, start: &[Anchor], pinned: &PortPlacements, axes: Axes, effort: RelaxEffort) -> Result<ContinuousPlacement, RelaxError>`

- [ ] **Step 1: Write the failing tests**

Add to `src/compile/relax/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::library::Library;
    use crate::compile::primitive_graph::expand;
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    fn relaxed(netlist: &Netlist, effort: RelaxEffort) -> ContinuousPlacement {
        let graph = expand(netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..netlist.gates.len() + netlist.inputs.len())
            .map(|index| Anchor { x: index as i32 * 20, y: 1, z: index as i32 * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[netlist.gates.len()]);
        relax(netlist, &graph, &start, &placements, Axes::IN_PLANE, effort)
            .expect("a two-gate chain relaxes")
    }

    /// Same graph, same effort, identical placement -- bit for bit, not
    /// nearly. Every measurement taken downstream is noise otherwise.
    #[test]
    fn the_same_input_relaxes_to_the_same_bits() {
        let netlist = chain();
        let effort = RelaxEffort { iterations: 64, seed: 0x26_02 };
        let first = relaxed(&netlist, effort);
        let second = relaxed(&netlist, effort);

        for (index, (left, right)) in first.graph.bodies.iter().zip(&second.graph.bodies).enumerate()
        {
            assert_eq!(
                left.position.map(f64::to_bits),
                right.position.map(f64::to_bits),
                "body {index} landed somewhere else the second time"
            );
            assert_eq!(left.facing, right.facing, "body {index} turned differently");
        }
    }

    /// Torque produces orientation. A repeater whose only consumer sits east
    /// ends up driving its front eastwards -- stated as geometry rather than
    /// as a compass bearing, since "faces east" means different things for a
    /// wall torch and a repeater.
    ///
    /// Hand-built rather than taken from a circuit, because this is the claim
    /// the whole facing mechanism rests on and it has to be checkable by
    /// reading it.
    #[test]
    fn a_body_turns_to_face_what_pulls_it() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        // `b`'s only consumer, `c`, sits far to the east of it.
        let start = vec![
            Anchor { x: 0, y: 1, z: 0 },   // gate b
            Anchor { x: 60, y: 1, z: 0 },  // gate c
            Anchor { x: -20, y: 1, z: 0 }, // input a
        ];
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);
        placements.pin("c", start[1]);

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &placements,
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        let b = placement.graph.anchor_body[0];
        assert_eq!(
            placement.graph.bodies[b].facing.direction(),
            crate::redstone::world::block::Facing::East,
            "b's output has to leave towards the only thing reading it"
        );
    }

    /// A relaxation that ran out of iterations says so rather than handing
    /// back something that looks placed.
    #[test]
    fn running_out_of_iterations_is_an_error_that_names_the_worst_pair() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let error = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort { iterations: 1, seed: 0 },
        )
        .expect_err("one iteration from a knot cannot converge");
        assert!(matches!(error, RelaxError::DidNotConverge { iterations: 1, .. }));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::tests`

Expected: compile failure -- `relax`, `RelaxEffort`, `ContinuousPlacement` do
not exist.

- [ ] **Step 3: Write the errors and the effort**

Add to `src/compile/relax/mod.rs`:

```rust
mod build;
mod linear;
mod project;

pub use build::{attach_offset, Attach, Body, BodyGraph, BodyKind, Pull, Weld, SIGNAL_STIFFNESS};
pub use project::{reservation, Axes, Violation, CONDUCTOR_CLEARANCE, SNAP_MARGIN};

use crate::compile::planner::{Anchor, PortPlacements};
use crate::compile::primitive_graph::PrimitiveGraph;
use crate::compile::Netlist;
use linear::Factorisation;

/// How far a body may still be moving and the relaxation still be finished.
///
/// A tenth of a cell, because the rounding margin is a whole one: a system
/// still twitching below that cannot change what `snap` produces, and running
/// past it buys nothing measurable.
pub const CONVERGED: f64 = 0.1;

/// How hard to try, and from where.
///
/// The seed has one job: retrying a stuck configuration from a slightly
/// different one, reproducibly. It is *not* what breaks the planar symmetry --
/// upward separation does that, in Stage 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelaxEffort {
    pub iterations: usize,
    pub seed: u64,
}

impl Default for RelaxEffort {
    fn default() -> Self {
        RelaxEffort { iterations: 256, seed: 0 }
    }
}

/// A relaxed placement, in continuous space, before anything is rounded.
#[derive(Debug, Clone)]
pub struct ContinuousPlacement {
    pub graph: BodyGraph,
    /// Whether the last step moved every body less than [`CONVERGED`].
    ///
    /// `snap` refuses an unconverged placement: rounding is exact only if the
    /// projection converged, and one that did not has no margin to spend.
    pub converged: bool,
    pub iterations: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelaxError {
    /// The budget ran out with a violation still standing.
    DidNotConverge { iterations: usize, worst: Violation },
    /// No progress, and a violation still standing. A different error because
    /// the remedy differs: constraints that contradict, not a budget that ran
    /// out.
    Deadlocked { worst: Violation },
    /// A connected component with nothing pinned in it: it may slide freely,
    /// so the system has no unique answer.
    Unsolvable { component_row: usize },
    /// The netlist and its primitive graph do not agree well enough to build
    /// bodies from -- a gate with no primitive, a declared input with no
    /// lever.
    CannotBuild { reason: String },
}

impl std::fmt::Display for RelaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelaxError::DidNotConverge { iterations, worst } => write!(
                f,
                "relaxation did not converge in {iterations} iterations; bodies {} and {} are {:.3} too close",
                worst.left, worst.right, worst.shortfall
            ),
            RelaxError::Deadlocked { worst } => write!(
                f,
                "projection deadlocked: bodies {} and {} cannot be {:.3} further apart and stay welded",
                worst.left, worst.right, worst.shortfall
            ),
            RelaxError::Unsolvable { component_row } => write!(
                f,
                "the spring system has no unique answer at body {component_row}: nothing in its component is pinned"
            ),
            RelaxError::CannotBuild { reason } => {
                write!(f, "cannot build bodies for this netlist: {reason}")
            }
        }
    }
}

impl std::error::Error for RelaxError {}
```

- [ ] **Step 4: Write the matrix assembly**

```rust
/// The weighted Laplacian, with pinned bodies struck out.
///
/// Struck out rather than weighted heavily: a pinned body takes no force, so
/// it is not an unknown, and its position moves to the right-hand side. That
/// also makes the matrix positive definite instead of merely semi-definite,
/// which is what lets [`Factorisation`] refuse an unpinned component rather
/// than return one of its infinitely many answers.
fn laplacian(graph: &BodyGraph, free: &[Option<usize>], order: usize) -> Vec<f64> {
    let mut matrix = vec![0.0; order * order];
    for pull in &graph.pulls {
        let (left, right) = (pull.from.0, pull.to.0);
        match (free[left], free[right]) {
            (Some(i), Some(j)) => {
                matrix[i * order + i] += pull.stiffness;
                matrix[j * order + j] += pull.stiffness;
                matrix[i * order + j] -= pull.stiffness;
                matrix[j * order + i] -= pull.stiffness;
            }
            (Some(i), None) | (None, Some(i)) => {
                matrix[i * order + i] += pull.stiffness;
            }
            (None, None) => {}
        }
    }
    matrix
}

/// The right-hand side for one axis, given the current facings.
///
/// A pull wants `(x_i + off_i) - (x_j + off_j) == 0`, so the port offsets and
/// every pinned neighbour's position land here while the matrix stays the
/// same. That is the whole reason the factorisation is computed once: facings
/// change the offsets, and offsets are in `b`, not in `A`.
fn right_hand_side(graph: &BodyGraph, free: &[Option<usize>], order: usize, axis: usize) -> Vec<f64> {
    let mut rhs = vec![0.0; order];
    for pull in &graph.pulls {
        let (left, right) = (pull.from.0, pull.to.0);
        let left_offset = attach_offset(pull.from.1, &graph.bodies[left])[axis];
        let right_offset = attach_offset(pull.to.1, &graph.bodies[right])[axis];
        let want = right_offset - left_offset;

        if let Some(i) = free[left] {
            rhs[i] += pull.stiffness * want;
            if free[right].is_none() {
                rhs[i] += pull.stiffness * graph.bodies[right].position[axis];
            }
        }
        if let Some(j) = free[right] {
            rhs[j] -= pull.stiffness * want;
            if free[left].is_none() {
                rhs[j] += pull.stiffness * graph.bodies[left].position[axis];
            }
        }
    }
    rhs
}
```

- [ ] **Step 5: Write the facing enumeration**

```rust
/// Each body's best facing for the current positions, found by trying all
/// four.
///
/// Not a rotation integrated over time: an enumeration, because there are
/// four. Ties go to the lowest index so the same input always turns the same
/// way.
///
/// Returns whether anything turned, because a step that changed no facing and
/// moved nothing is a converged step.
fn choose_facings(graph: &mut BodyGraph) -> bool {
    let mut turned = false;
    for body in 0..graph.bodies.len() {
        if matches!(graph.bodies[body].what, BodyKind::Junction { .. })
            && graph.bodies[body].pinned
        {
            continue;
        }
        let mut best = (graph.bodies[body].facing, f64::INFINITY);
        for index in 0..4u8 {
            let facing = crate::compile::geometry::CellFacing::from_index(index)
                .expect("0..4 is horizontal");
            graph.bodies[body].facing = facing;
            let energy = incident_energy(graph, body);
            if energy < best.1 {
                best = (facing, energy);
            }
        }
        if graph.bodies[body].facing != best.0 {
            turned = true;
        }
        graph.bodies[body].facing = best.0;
    }
    turned
}

/// The spring energy of every pull touching `body`, with everything else held.
fn incident_energy(graph: &BodyGraph, body: usize) -> f64 {
    let mut energy = 0.0;
    for pull in &graph.pulls {
        if pull.from.0 != body && pull.to.0 != body {
            continue;
        }
        let from = &graph.bodies[pull.from.0];
        let to = &graph.bodies[pull.to.0];
        let from_at = attach_offset(pull.from.1, from);
        let to_at = attach_offset(pull.to.1, to);
        let mut squared = 0.0;
        for axis in 0..3 {
            let delta = (from.position[axis] + from_at[axis]) - (to.position[axis] + to_at[axis]);
            squared += delta * delta;
        }
        energy += pull.stiffness * squared;
    }
    energy
}
```

- [ ] **Step 6: Write `relax`**

```rust
/// Solve, turn, project. Repeat until nothing moves.
pub fn relax(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
    axes: Axes,
    effort: RelaxEffort,
) -> Result<ContinuousPlacement, RelaxError> {
    let mut bodies = build::build(netlist, graph, start, pinned)
        .map_err(|reason| RelaxError::CannotBuild { reason })?;
    perturb(&mut bodies, effort.seed);

    let mut free = vec![None; bodies.bodies.len()];
    let mut order = 0;
    for (index, body) in bodies.bodies.iter().enumerate() {
        if !body.pinned {
            free[index] = Some(order);
            order += 1;
        }
    }

    let required = project::required_separations(&bodies);
    let factorisation = Factorisation::of(&laplacian(&bodies, &free, order), order)
        .map_err(|error| RelaxError::Unsolvable { component_row: error.row })?;

    for iteration in 1..=effort.iterations {
        let before: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();

        for axis in 0..3 {
            let mut rhs = right_hand_side(&bodies, &free, order, axis);
            factorisation.solve(&mut rhs);
            for (index, slot) in free.iter().enumerate() {
                if let Some(slot) = slot {
                    bodies.bodies[index].position[axis] = rhs[*slot];
                }
            }
        }

        let turned = choose_facings(&mut bodies);

        if let Err(worst) = project::project(&mut bodies, &required, axes) {
            return Err(RelaxError::Deadlocked { worst });
        }

        let moved = before
            .iter()
            .zip(&bodies.bodies)
            .map(|(was, body)| {
                (0..3)
                    .map(|axis| (was[axis] - body.position[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        if moved < CONVERGED && !turned {
            return Ok(ContinuousPlacement {
                graph: bodies,
                converged: true,
                iterations: iteration,
            });
        }
    }

    let worst = project::worst_violation(&bodies, &required).unwrap_or(Violation {
        left: 0,
        right: 0,
        shortfall: 0.0,
    });
    Err(RelaxError::DidNotConverge {
        iterations: effort.iterations,
        worst,
    })
}

/// Nudge the start, reproducibly.
///
/// A stuck configuration can be retried from a slightly different one. Seed
/// zero is no perturbation at all, which is what every measurement in this
/// design is taken with.
fn perturb(graph: &mut BodyGraph, seed: u64) {
    if seed == 0 {
        return;
    }
    let mut state = seed;
    for body in &mut graph.bodies {
        if body.pinned {
            continue;
        }
        for axis in [0usize, 2] {
            // splitmix64, so a seed of one bit still moves every body.
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            // A quarter cell either way: enough to break a tie, not enough to
            // move a body past a neighbour.
            body.position[axis] += (z >> 11) as f64 / (1u64 << 53) as f64 * 0.5 - 0.25;
        }
    }
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test --release --lib compile::relax::tests`

Expected: 3 passed.

- [ ] **Step 8: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(relax): a step is a solve, a choice among four, and a projection"
```

---

### Task 9: Landing on the lattice, and refusing to when it would lie

`snap` rounds positions and collapses each gate's bodies to the one anchor
`PlanCandidate` has room for. It returns a `Result` for one reason: rounding is
exact only if the projection converged, and a placement handed here
unconverged has no margin to spend. Rounding it anyway would produce exactly
the class of failure this design exists to avoid -- a layout that looks placed
and is illegal in ways the invariants find later and attribute elsewhere.

**Files:**
- Create: `src/compile/relax/snap.rs`
- Modify: `src/compile/relax/mod.rs`
- Test: `src/compile/relax/snap.rs`

**Interfaces:**
- Consumes: `ContinuousPlacement`, `RelaxError`, `project::{required_separations, worst_violation}`, `planner::Anchor`, `geometry::CellFacing`.
- Produces:
  - `pub struct SnappedNode { pub node: usize, pub anchor: Anchor, pub facing: CellFacing }`
  - `pub fn snap(placement: &ContinuousPlacement) -> Result<Vec<SnappedNode>, RelaxError>`
  - `RelaxError` gains `SurvivedSnap { worst: Violation }`

`SnappedNode` is keyed by **candidate node index**, not by `NodeId`. The spec's
sketch said `NodeId`, and it cannot: a bare merge's junction has no node --
`expand` produces no primitive for one -- so there is no `NodeId` to name it
by. Gate index then input index is the order `emit_primitives` reads
positionally, which is the order the answer has to arrive in anyway.

- [ ] **Step 1: Write the failing tests**

Create `src/compile/relax/snap.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::library::Library;
    use crate::compile::primitive_graph::expand;
    use crate::compile::relax::{relax, Axes, RelaxEffort};
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    /// One answer per node `PlanCandidate` expects, in the order it expects
    /// them: gates, then primary inputs.
    #[test]
    fn snap_answers_once_per_candidate_node_in_candidate_order() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);

        let placement = relax(&netlist, &graph, &start, &placements, Axes::IN_PLANE, RelaxEffort::default())
            .expect("relaxes");
        let snapped = snap(&placement).expect("a converged placement rounds");

        assert_eq!(snapped.len(), 3);
        for (index, node) in snapped.iter().enumerate() {
            assert_eq!(node.node, index, "answers are not in candidate order");
        }
    }

    /// A pinned port comes back exactly where it was pinned. Not near it.
    #[test]
    fn a_pinned_port_snaps_to_where_it_was_pinned() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let pinned_at = Anchor { x: 37, y: 1, z: 41 };
        let mut placements = PortPlacements::default();
        placements.pin("a", pinned_at);

        let placement = relax(&netlist, &graph, &start, &placements, Axes::IN_PLANE, RelaxEffort::default())
            .expect("relaxes");
        let snapped = snap(&placement).expect("rounds");

        assert_eq!(snapped[2].anchor, pinned_at, "input `a` was pinned");
    }

    /// An unconverged placement is refused rather than rounded. The margin it
    /// would spend is not there.
    #[test]
    fn an_unconverged_placement_is_refused_rather_than_rounded() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let placement = ContinuousPlacement {
            graph: crate::compile::relax::build::build(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
            )
            .expect("builds"),
            converged: false,
            iterations: 1,
        };

        let error = snap(&placement).expect_err("an unconverged placement has no margin");
        assert!(matches!(error, RelaxError::DidNotConverge { .. }));
    }

    /// Rounding moves a body by at most half a cell, so two approach by at
    /// most one -- which is what `SNAP_MARGIN` claims. The case that tests it
    /// is one sitting on a half-cell boundary in every axis at once, where
    /// rounding moves a body furthest.
    #[test]
    fn separation_survives_rounding_from_a_half_cell_boundary() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = crate::compile::relax::project::required_separations(&built);
        let gap = required[0].max(required[1]) + crate::compile::relax::SNAP_MARGIN;
        built.bodies[0].position = [0.5, 1.5, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.5, 0.5];
        built.bodies[2].position = [0.5 + 2.0 * gap, 1.5, 0.5];

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        snap(&placement).expect("a solution separated by the requirement plus one survives");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::snap`

Expected: compile failure -- `snap` and `SnappedNode` do not exist.

- [ ] **Step 3: Write `snap`**

Create `src/compile/relax/snap.rs`:

```rust
//! Rounding a relaxed placement onto the lattice, and refusing to when that
//! would be a lie.
//!
//! There is no facing to quantise here. The solver chose one of four at every
//! step, so what is left is rounding positions -- and one cell of margin
//! covers that, because rounding moves a body by at most half a cell, so two
//! bodies approach by at most one.

use crate::compile::geometry::CellFacing;
use crate::compile::planner::Anchor;
use crate::compile::relax::build::BodyKind;
use crate::compile::relax::project::{required_separations, worst_violation};
use crate::compile::relax::{ContinuousPlacement, RelaxError};

/// Where one of `PlanCandidate`'s nodes goes, and which way it is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnappedNode {
    /// Index into `PlanCandidate`'s nodes: gates first, then primary inputs.
    ///
    /// Not a `NodeId`. A bare merge's junction has no node -- `expand`
    /// produces no primitive for one -- so there would be nothing to name it
    /// by, and this is the order `emit_primitives` reads anyway.
    pub node: usize,
    pub anchor: Anchor,
    pub facing: CellFacing,
}

/// Round a converged placement onto the lattice.
pub fn snap(placement: &ContinuousPlacement) -> Result<Vec<SnappedNode>, RelaxError> {
    if !placement.converged {
        let required = required_separations(&placement.graph);
        let worst = worst_violation(&placement.graph, &required).unwrap_or(
            crate::compile::relax::Violation { left: 0, right: 0, shortfall: 0.0 },
        );
        return Err(RelaxError::DidNotConverge {
            iterations: placement.iterations,
            worst,
        });
    }

    let mut rounded = placement.graph.clone();
    for body in &mut rounded.bodies {
        for axis in 0..3 {
            body.position[axis] = body.position[axis].round();
        }
    }

    // The margin's claim, checked rather than trusted. The invariants exist to
    // catch real errors, not to catch the legaliser's leftovers.
    let required = required_separations(&rounded);
    if let Some(worst) = worst_violation(&rounded, &required) {
        return Err(RelaxError::SurvivedSnap { worst });
    }

    Ok(rounded
        .anchor_body
        .iter()
        .enumerate()
        .map(|(node, &body)| SnappedNode {
            node,
            anchor: Anchor {
                x: rounded.bodies[body].position[0] as i32,
                y: rounded.bodies[body].position[1] as i32,
                z: rounded.bodies[body].position[2] as i32,
            },
            facing: rounded.bodies[body].facing,
        })
        .collect())
}
```

The collapse is the `anchor_body` lookup and nothing more. A gate's other
bodies -- an isolated merge's branch repeaters -- are welded to that anchor, so
their relaxed positions are never something this throws away: the weld never
let them be anywhere else.

- [ ] **Step 4: Add the error variant**

In `src/compile/relax/mod.rs`, add to `RelaxError` and its `Display`:

```rust
    /// A violation survived rounding, so the margin argument is wrong.
    ///
    /// Reported here rather than left for an invariant, which would find it
    /// downstream and attribute it to routing.
    SurvivedSnap { worst: Violation },
```

```rust
            RelaxError::SurvivedSnap { worst } => write!(
                f,
                "rounding left bodies {} and {} {:.3} too close: the snap margin is wrong",
                worst.left, worst.right, worst.shortfall
            ),
```

- [ ] **Step 5: Declare the module**

`src/compile/relax/mod.rs` gains `mod snap;` and
`pub use snap::{snap, SnappedNode};`.

- [ ] **Step 6: Run the tests**

Run: `cargo test --release --lib compile::relax::snap`

Expected: 4 passed.

- [ ] **Step 7: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`.

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat(relax): snapping rounds a converged placement and refuses any other"
```

---

### Task 10: `plan_from_netlist` places by relaxation

The question the whole design exists for gets answered here: does relaxation
place better than rows and barycentres? 572 blocks and 24 game ticks for and4
are the numbers to beat.

**Files:**
- Modify: `src/compile/planner.rs` -- `plan_from_netlist_shaped` (1797-1927), `emit_primitives` (562-677), `PlanCandidate::with_primitive_nodes` (332-347)
- Test: `src/compile/planner.rs`

**Interfaces:**
- Consumes: `relax::{relax, snap, Axes, RelaxEffort, SnappedNode, RelaxError}`.
- Produces:
  - `PlanCandidate::with_facings(anchors, primitive_nodes, routes, facings: Vec<CellFacing>) -> Self` -- the constructor `variant_indices` never had.
  - `PlannerError` gains `Relaxation(RelaxError)`.
  - `plan_from_netlist` and `plan_from_netlist_shaped` keep their signatures.
  - `fn starting_layout(netlist: &Netlist, placements: &PortPlacements, shape: Shape) -> Result<Vec<Anchor>, PlannerError>` -- the existing depth-and-barycentre code, extracted verbatim.

- [ ] **Step 1: Write the failing tests**

Add to `planner.rs`'s test module:

```rust
/// Corridors exist: a relaxed placement is not merely legal but routable.
///
/// This is what the routing reservation claims, and it is the term with no
/// precedent to lean on -- legacy reserves routing space by construction and
/// this does it by a number that was guessed.
///
/// "Could reach from the old placement" rather than "every sink" because
/// segment_a and above do not route today whatever places them, and this test
/// is about placement.
#[test]
fn relaxation_routes_everything_the_old_placement_could() {
    use crate::circuits::full_adder::build_full_adder_netlist;

    for (name, netlist) in [
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
    ] {
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
            .unwrap_or_else(|error| panic!("{name} must place: {error}"));
        verify_candidate(&candidate, &netlist)
            .unwrap_or_else(|error| panic!("{name} must be legal: {error}"));
    }
}

/// Better than what it replaced, on both counts.
///
/// Both, because rows and barycentres are already smaller than they were and
/// slower than the emitter -- beating one by giving up the other is not an
/// improvement, it is a different trade.
#[test]
fn relaxation_places_and4_smaller_and_faster_than_rows_and_barycentres() {
    let (netlist, _) = build_and4_netlist();
    let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
    let realised = realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate))
        .expect("is legal");

    // Counted the way `the_hand_written_circuits_keep_their_measured_size`
    // counts, so the numbers mean the same thing as the 472 it pins.
    let (size_x, size_y, size_z) = realised.world.size();
    let mut blocks = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if realised.world.get(x, y, z).kind != BlockKind::Air {
                    blocks += 1;
                }
            }
        }
    }
    // The candidate's own delay, in game ticks -- the term
    // `measure_optimisation_at_scale` prints and the one the 24 came from.
    let settle = candidate.cost().delay;

    // Rows and barycentres, measured 2026-08-12.
    assert!(blocks < 572, "relaxation placed {blocks} blocks against 572");
    assert!(settle < 24, "relaxation settled in {settle} ticks against 24");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::planner::tests::relaxation`

Expected: both fail -- the first because `plan_from_netlist` still places by
rows, the second on the two assertions, since rows and barycentres produce
exactly 572 and 24.

- [ ] **Step 3: Extract the starting layout**

Move `plan_from_netlist_shaped`'s body (1803-1873, ending at the `by_depth`
loop) into:

```rust
/// Gates in rows by depth, one signal per column, primary inputs one row past
/// the deepest.
///
/// Kept, because relaxation starts from it. A spring system with hard
/// constraints is not convex, so the starting point decides which solution it
/// finds: starting everything at the origin gives a knot the projection has to
/// unpick, and starting at random makes the result unreproducible. This layout
/// is legal, reproducible, and measurably poor -- which makes relaxation's job
/// "improve a known-bad answer" rather than "invent one", and its improvement
/// measurable against the numbers it started from.
fn starting_layout(
    netlist: &Netlist,
    placements: &PortPlacements,
    shape: Shape,
) -> Result<Vec<Anchor>, PlannerError> {
```

returning one `Anchor` per candidate node -- gates, then primary inputs -- built
from `gate_x`/`gate_storey`/`row` and `input_x` exactly as lines 1878-1923 do
today, but without building `PrimitiveNode`s.

- [ ] **Step 4: Relax, snap, and build the candidate**

```rust
pub fn plan_from_netlist_shaped(
    netlist: &Netlist,
    placements: &PortPlacements,
    shape: Shape,
) -> Result<PlanCandidate, PlannerError> {
    let start = starting_layout(netlist, placements, shape)?;
    let graph = primitive_graph::expand(netlist, &Library::default_library())
        .map_err(|error| PlannerError::UnrealisableNode {
            id: "netlist".to_string(),
            reason: error.to_string(),
        })?;

    let placement = relax::relax(
        netlist,
        &graph,
        &start,
        placements,
        relax::Axes::IN_PLANE,
        relax::RelaxEffort::default(),
    )
    .map_err(PlannerError::Relaxation)?;
    let snapped = relax::snap(&placement).map_err(PlannerError::Relaxation)?;

    let mut anchors = Vec::with_capacity(snapped.len());
    let mut facings = Vec::with_capacity(snapped.len());
    let mut primitive_nodes = Vec::with_capacity(snapped.len());

    for node in &snapped {
        anchors.push(node.anchor);
        facings.push(node.facing);
    }

    for (index, gate) in netlist.gates.iter().enumerate() {
        let anchor = anchors[index];
        let (footprint, conductors, output_pin) =
            compile::gate_footprint((anchor.x, anchor.y, anchor.z), gate, facings[index]);
        primitive_nodes.push(PrimitiveNode {
            id: format!("gate:{}", gate.output),
            anchor,
            realisation: if gate.is_merge() {
                NodeRealisation::WireMerge
            } else {
                NodeRealisation::Primitive(Primitive::Torch)
            },
            footprint,
            conductors,
            pinned: placements.get(&gate.output).is_some(),
            output_pin: Some(output_pin),
        });
    }

    for (index, input) in netlist.inputs.iter().enumerate() {
        let node = netlist.gates.len() + index;
        let anchor = anchors[node];
        let pin = step(anchor, compile::geometry::output_direction(facings[node]));
        primitive_nodes.push(PrimitiveNode {
            id: format!("input:{input}"),
            anchor,
            realisation: NodeRealisation::Primitive(Primitive::Lever),
            footprint: vec![anchor, pin],
            conductors: vec![anchor, pin],
            pinned: placements.get(input).is_some(),
            output_pin: Some(pin),
        });
    }

    let candidate =
        PlanCandidate::with_facings(anchors, primitive_nodes, Vec::new(), facings);
    route_every_net(candidate, netlist)
}
```

- [ ] **Step 5: Give `PlanCandidate` a constructor that sets facings**

```rust
    /// Construct a candidate whose nodes are not all built facing north.
    ///
    /// `variant_indices` has existed since the candidate model landed and
    /// every constructor has filled it with zeroes. This is the one that puts
    /// something in it.
    pub fn with_facings(
        anchors: Vec<Anchor>,
        primitive_nodes: Vec<PrimitiveNode>,
        routes: Vec<Route>,
        facings: Vec<geometry::CellFacing>,
    ) -> Self {
        assert_eq!(facings.len(), anchors.len(), "one facing per anchor");
        let mut candidate = PlanCandidate::with_primitive_nodes(anchors, primitive_nodes, routes);
        candidate.variant_indices = facings.iter().map(|facing| facing.index()).collect();
        candidate
    }
```

- [ ] **Step 6: Make `emit_primitives` build to the recorded facing**

At `planner.rs:613` and `621`, pass `candidate.facing_of(index)` to
`place_merge_gate` / `place_nor_gate` and to `output_pin`. At `641`, pass it to
`place_primary_input`. This is where Task 3's `facing_of` stops returning north
for everything.

- [ ] **Step 7: Settle the socket disagreement Task 3 left**

`try_move`'s `terminal_socket` (1559) picks a socket from the source-to-support
delta; `route_in_order` picks `input_directions(facing)[input_index]`. With
facings varying they disagree more often, and the router's answer is the
correct one -- it is the socket the netlist declared, and `equivalence` checks
exactly that. Delete `terminal_socket`/`preferred_axis_direction` and have
`try_move`'s rebuild loop (776) and `3397` call the same
`input_directions(facing)[input_index]` lookup.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::planner::tests::relaxation`

Expected: both PASS. If the second fails, record the numbers it did produce in
the commit message: the design's own condition is that failing to beat 572 and
24 means it failed at the thing it was written for, and that is a result to
report rather than a test to weaken.

- [ ] **Step 9: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. `compile()` still seeds from legacy, so the four pinned
sizes are unchanged. `how_far_the_planners_own_placement_carries` is still
ignored; run it by hand to see how far relaxation now carries:

```bash
cargo test --release --lib compile::planner::tests::how_far_the_planners_own_placement_carries -- --ignored --nocapture
```

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(planner): placement is a relaxation, not a row and a barycentre"
```

**Stage 1 is done when** `plan_from_netlist` places by relaxation, and4 is
smaller and faster than the 572/24 it started from, and `compile()` has not
moved.

---

# Stage 2 -- separation that may push upwards

### Task 11: Crowding buys height, and `Shape::Tall` stops asking for it

One word changes: `Axes::IN_PLANE` becomes `Axes::ALL`. Everything else in this
task is removing the hand-made rule that was standing in for it.

Height gets used at all because the vertical requirement is smaller than the
horizontal one -- [`CONDUCTOR_CLEARANCE`] flat, with no routing reservation on
top. A body with nowhere to go sideways has somewhere to go up, and it is
cheaper. That asymmetry is already in `shortfall` from Task 7; this task is
what lets `separate` act on it.

**Files:**
- Modify: `src/compile/planner.rs` -- `Shape` (1731-1751) deleted, `plan_from_netlist_shaped` merged into `plan_from_netlist`, `TALL_COLUMN_LIMIT` (1754) deleted, the test at 4299-4329 replaced
- Modify: `src/compile/relax/project.rs` -- nothing; `Axes::ALL` already exists
- Test: `src/compile/planner.rs`

**Interfaces:**
- Consumes: `relax::Axes::ALL`.
- Produces: `plan_from_netlist(netlist, placements)` is the only entry point.
  `Shape`, `plan_from_netlist_shaped` and `TALL_COLUMN_LIMIT` are **deleted**.
  `starting_layout` loses its `shape` parameter and always lays one storey.

- [ ] **Step 1: Write the failing tests**

Replace `a_tall_preference_uses_height_where_a_wide_one_uses_floor` in
`planner.rs`'s test module with:

```rust
/// A stacked pair is already separated, and the projection moves neither.
///
/// This is the vertical exemption read straight off the safety condition: its
/// unsafe case needs a horizontal cardinal step, so there is no pure-vertical
/// case at all. It is the mechanism the next test buys with, tested where it
/// can be seen rather than inferred from a layout.
#[test]
fn two_bodies_in_one_column_are_left_where_they_are() {
    use crate::compile::relax::{project_for_test, Axes, CONDUCTOR_CLEARANCE};

    let mut graph = project_for_test::two_free_bodies(
        [10.0, 1.0, 10.0],
        [10.0, 1.0 + CONDUCTOR_CLEARANCE, 10.0],
    );
    let required = vec![9.0, 9.0];
    project_for_test::project(&mut graph, &required, Axes::ALL).expect("already separated");

    assert_eq!(graph.bodies[0].position, [10.0, 1.0, 10.0]);
    assert_eq!(graph.bodies[1].position, [10.0, 1.0 + CONDUCTOR_CLEARANCE, 10.0]);
}

/// Height is earned by crowding rather than requested.
///
/// Six gates that all consume one signal have every reason to sit near it and
/// no room to. Spreading sideways costs the full horizontal requirement,
/// reservations and all; stacking costs two cells. So they stack.
///
/// This replaces `a_tall_preference_uses_height_where_a_wide_one_uses_floor`
/// and claims less than it did: not "ask for tall and get tall", but "crowd it
/// and it stacks".
#[test]
fn crowding_produces_height() {
    let netlist = six_independent_gates();
    let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
        .expect("six gates on one signal must place");
    verify_candidate(&candidate, &netlist).expect("must be legal");

    let (_, height, _) = extent(&candidate);
    assert!(height > 1, "six crowded gates stayed on one level");
}
```

`project_for_test` is a `#[cfg(test)] pub(crate)` re-export module in
`relax/mod.rs` exposing `project`, `required_separations` and a
`two_free_bodies` fixture, so `planner`'s tests can reach the projection
without making it public.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::planner::tests::crowding_produces_height`

Expected: FAIL -- relaxation is still in-plane, so six gates spread sideways
and `height` is 1.

- [ ] **Step 3: Let separation reach for the third dimension**

In `plan_from_netlist`, `relax::Axes::IN_PLANE` becomes `relax::Axes::ALL`.

- [ ] **Step 4: Delete the knob**

Delete `Shape`, its `Default`, `TALL_COLUMN_LIMIT`, and `plan_from_netlist_shaped`.
Fold its body into `plan_from_netlist`. In `starting_layout`, delete the
`storey` match (1841-1845) and the `(row, storey)` key becomes `row`; every
gate starts on one plane and separation decides whether it stays there.

Keep `STOREY_PITCH` -- it is what a pinned anchor's Y is measured against at
1849, and pinning still works.

- [ ] **Step 5: Run the tests**

Run: `cargo test --release --lib compile::planner::tests`

Expected: PASS, including both new tests.

- [ ] **Step 6: Re-run the corridor test at height**

Run: `cargo test --release --lib compile::planner::tests::relaxation_routes_everything_the_old_placement_could`

Expected: PASS. This is the one that can regress here: routes need floors, and
a body that moved up may now sit where a floor has to go. If it fails, the
routing reservation is what to look at first -- the vertical requirement
carries none, which is deliberate and is exactly the assumption under test.

- [ ] **Step 7: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`, one fewer test than before (the shape test was replaced
by two, so one more).

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat(relax): height is what crowding buys, not what a knob asks for"
```

---

### Task 12: Native and wasm place the same circuit

`viewer/` compiles the same crate to wasm, and once `compile()` uses this
placer the circuit drawn in a browser is the circuit this code placed. If the
two toolchains disagree in the last bits, the same netlist yields two layouts
and the viewer stops being evidence about the compiler.

This is stated as a risk with a test rather than settled by argument, because
which way it goes is a fact about two toolchains.

**Files:**
- Create: `viewer/tests/placement_agrees_with_native.rs`
- Modify: `src/compile/planner.rs` -- a `pub fn placement_fingerprint(candidate: &PlanCandidate) -> String`
- Test: both of the above

**Interfaces:**
- Produces: `pub fn placement_fingerprint(candidate: &PlanCandidate) -> String` -- every anchor and facing in candidate-node order, as text, so two toolchains can be compared by string rather than by float.

- [ ] **Step 1: Write the fingerprint and the native expectation**

```rust
/// Every anchor and facing this candidate chose, in candidate-node order.
///
/// Text rather than a hash: when two toolchains disagree, the useful output is
/// which node moved, not that something did.
pub fn placement_fingerprint(candidate: &PlanCandidate) -> String {
    candidate
        .anchors()
        .iter()
        .enumerate()
        .map(|(node, anchor)| {
            format!(
                "{node} {} {} {} {}",
                anchor.x,
                anchor.y,
                anchor.z,
                candidate.facing_of(node).index()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

- [ ] **Step 2: Write the failing test**

`viewer/tests/placement_agrees_with_native.rs`:

```rust
//! The circuit the browser draws is the circuit the compiler placed.
//!
//! `viewer/` builds the same crate for wasm. `f64` addition, multiplication
//! and `sqrt` are exact IEEE-754 operations and the solver contracts no FMA,
//! so the two should agree bit for bit -- but "should" is an argument and this
//! is a test. If it fails, the positions become fixed-point: the arithmetic is
//! addition, multiplication and comparison, all of which fixed-point does
//! exactly, and the only thing lost is the convenience of `f64` in the
//! projection.

use reda::circuits::and4::build_and4_netlist;
use reda::compile::planner::{placement_fingerprint, plan_from_netlist, PortPlacements};

#[test]
fn and4_places_identically_wherever_it_is_built() {
    let (netlist, _) = build_and4_netlist();
    let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
    let fingerprint = placement_fingerprint(&candidate);

    let expected = include_str!("fixtures/and4_placement.txt");
    assert_eq!(
        fingerprint.trim(),
        expected.trim(),
        "this toolchain placed and4 somewhere else"
    );
}
```

- [ ] **Step 3: Run it natively to produce the fixture**

Run the test once with the fixture absent, capture the fingerprint it computed,
and write it to `viewer/tests/fixtures/and4_placement.txt`. The fixture is the
native answer; the test's job is to make any other toolchain disagree loudly.

- [ ] **Step 4: Run it under wasm**

Run:

```bash
cd viewer && wasm-pack test --node -- --test placement_agrees_with_native
```

Expected: PASS. **If it fails**, do not adjust the fixture. Record the
divergence, and convert `Body::position` and the solver to fixed-point
(`i64` at 1/1024 of a cell): the arithmetic is addition, multiplication and
comparison, and `sqrt` appears only in the Cholesky pivot, which can take an
integer square root. That is a separate task and this test is what would
justify it.

- [ ] **Step 5: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0` for both crates.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "test(viewer): the browser's circuit is the compiler's circuit, or this says so"
```

**Stage 2 is done when** six crowded gates stack, `Shape` is gone, and native
and wasm place and4 identically.

---

# Stage 3 -- the switchover

### Task 13: `compile()` places by relaxation

**Condition, from the spec:** all four hand-written circuits and both Verilog
circuits must place, route, verify, and match their truth tables. Not one
fewer.

If placement measurably improves and routing still fails at `segment_a`'s size,
the answer is **not** to weaken this condition. It is that routing became the
next piece of work. Say so, stop, and do not ship a `compile()` that only
handles small circuits.

Beating the legacy emitter is **not** a condition. The choice recorded in the
spec is to switch when it is correct, not when it is better.

**Files:**
- Modify: `src/compile/mod.rs` -- `compile` (6137, tail 6313-6335)
- Modify: `tests/reference_circuits.rs` -- `the_hand_written_circuits_keep_their_measured_size`
- Modify: `src/compile/planner.rs` -- un-ignore `how_far_the_planners_own_placement_carries`
- Test: `tests/reference_circuits.rs`, `tests/compile_end_to_end.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `compile()` returns a `CompiledCircuit` whose `legacy_emission` is
  `None`. `PlannerKind::Legacy` becomes reachable only through a test that
  builds both for comparison.

- [ ] **Step 1: Check the condition before changing anything**

Run:

```bash
cargo test --release --lib compile::planner::tests::how_far_the_planners_own_placement_carries -- --ignored --nocapture
```

Expected: all four of and4, full_adder, segment_a and seven_segment place and
verify. **If segment_a or seven_segment still fails, stop here.** Record what
failed and how far it got; that is the result, and Task 13 does not happen
until routing is fixed.

- [ ] **Step 2: Write the failing test**

In `tests/reference_circuits.rs`:

```rust
/// No reference circuit goes through the legacy emitter any more.
///
/// This asserts only the property the switchover is about. Truth tables need
/// no new test: every existing one in `tests/compile_end_to_end.rs`,
/// `tests/seven_segment.rs` and `tests/or_merge.rs` calls `compile()`, so they
/// exercise the new path the moment it lands -- and if any of them fails, the
/// condition for switching was not met and this task does not happen.
#[test]
fn no_reference_circuit_goes_through_the_legacy_emitter() {
    let circuits: [(&str, Netlist); 4] = [
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
        ("segment_a", build_single_segment_netlist(0).0),
        ("seven_segment", build_seven_segment_netlist().0),
    ];

    for (name, netlist) in circuits {
        let compiled =
            compile(&netlist).unwrap_or_else(|error| panic!("{name} must compile: {error}"));
        assert!(
            compiled.legacy_emission().is_none(),
            "{name} still went through the legacy emitter"
        );
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test --release --test reference_circuits no_reference_circuit_goes_through_the_legacy_emitter`

Expected: FAIL on the first circuit -- `compile` still seeds from legacy, so
`legacy_emission()` is `Some`.

- [ ] **Step 4: Rename the legacy path rather than deleting it**

The spec keeps legacy **as a comparison, not as the production path**. Deleting
it would leave Step 6 with nothing to compare against, so `compile`'s current
body -- realisability checks, `build_floorplan`, `build_nets`,
`reserve_columns`, `resolve_bypass_and_geometry`, and the seed handoff at
6324-6334 -- moves verbatim into:

```rust
/// Compile by the row/channel/track emitter.
///
/// No longer the production path: `compile` places by relaxation. Kept because
/// "is relaxation better" is a question somebody will ask again, and a
/// comparison nobody can run is a claim nobody can check.
///
/// This stamps `PlannerKind::Legacy`, which nothing constructed before today.
pub(crate) fn compile_legacy(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
```

with its final `PlannerKind::Unified3d` becoming `PlannerKind::Legacy` and its
`gate_facings` filled with north, which is what it builds.

- [ ] **Step 5: Point `compile` at the planner**

```rust
/// Compile a netlist into a verified circuit.
///
/// Placement is a spring relaxation; routing is A* with rip-up; the four
/// physical invariants pass on the world this returns, not on a second world
/// built separately from the same plan.
pub fn compile(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    compile_planned(netlist, &planner::PortPlacements::default())
}
```

`compile_planned` (6345-6361) gains the `gate_facings` field:

```rust
        gate_facings: (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect(),
```

The realisability, driven-signal and acyclicity checks `compile` did up front
(6138-6162) move into `compile_planned`, which had none of its own -- it relied
on `plan_from_netlist` failing later, with a worse message.

- [ ] **Step 6: Re-pin the sizes, and record what they were**

`the_hand_written_circuits_keep_their_measured_size` pins its expectations
inline (`tests/reference_circuits.rs:317-322`):

```rust
    let circuits: [(&str, Netlist, usize, usize); 4] = [
        ("and4", build_and4_netlist().0, 7, 472),
        ("full_adder", build_full_adder_netlist().0, 22, 1784),
        ("segment_a", build_single_segment_netlist(0).0, 46, 6416),
        ("seven_segment", build_seven_segment_netlist().0, 84, 16244),
    ];
```

Switching changes the fourth column, deliberately. Run the test once -- it
already `eprintln!`s the block count it measured for each circuit before
asserting -- and replace the four numbers with what it printed:

```bash
cargo test --release --test reference_circuits the_hand_written_circuits_keep_their_measured_size -- --nocapture
```

Then record the old numbers where the new ones now sit, so the test still says
where it has been:

```rust
    // Blocks measured at the switchover. The row/channel/track emitter
    // produced 472 / 1,784 / 6,416 / 16,244; these are what relaxation
    // produces. This test's meaning moves from "these must not change" to
    // "these were measured here, and changing them again needs an
    // explanation" -- which is what it was always for.
```

Do not guess the numbers, and do not adjust them to make a nicer story: if
relaxation is larger, the commit says so.

- [ ] **Step 7: Keep the comparison visible**

Beating legacy is not a condition, but forgetting the gap is not acceptable
either.

```rust
/// What the switchover cost or saved, printed rather than asserted.
///
/// The choice recorded in the spec is to switch when placement is correct, not
/// when it is better -- so a regression here is not a failure. It is a number
/// somebody has to be able to see.
#[test]
#[ignore = "measurement, not a gate"]
fn relaxation_against_the_emitter() {
    let circuits: [(&str, Netlist); 4] = [
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
        ("segment_a", build_single_segment_netlist(0).0),
        ("seven_segment", build_seven_segment_netlist().0),
    ];

    for (name, netlist) in circuits {
        let relaxed = compile(&netlist).expect("relaxation compiles");
        let legacy = compile_legacy(&netlist).expect("the emitter compiles");
        eprintln!(
            "{name}: relaxation {} blocks {:?}, emitter {} blocks {:?}",
            occupied(&relaxed.world),
            relaxed.world.size(),
            occupied(&legacy.world),
            legacy.world.size(),
        );
    }
}

/// Non-air cells, counted the way every size measurement in this file counts.
fn occupied(world: &World) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if world.get(x, y, z).kind != BlockKind::Air {
                    count += 1;
                }
            }
        }
    }
    count
}
```

`the_hand_written_circuits_keep_their_measured_size` counts non-air cells with
this same loop written inline; replace its copy with a call to `occupied` so
the two cannot drift apart.

- [ ] **Step 8: Un-ignore what now passes**

`how_far_the_planners_own_placement_carries` was ignored with "known: segment_a
needs a better search, not a looser rule". If Step 1 passed, remove the
`#[ignore]`. If Step 1 passed only because routing was fixed separately, say so
in the commit.

- [ ] **Step 9: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. `ignored` loses
`how_far_the_planners_own_placement_carries` and gains
`relaxation_against_the_emitter`, so the count is unchanged at 3 -- and
`candidate_delay_is_exact_for_a_circuit_with_fanout` and
`measure_optimisation_at_scale` are the other two, both still ignored for the
reasons they already carry.

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(compile): the circuit that ships is placed by relaxation"
```

**Stage 3 is done when** every reference circuit compiles with no
`legacy_emission`, every truth table matches, and the pinned sizes say what
relaxation measured with the emitter's numbers recorded beside them.

---

## Out of scope

- **Routing.** The A* and rip-up path stays as it is, with the one exception
  this design forces: the router is told the facing relaxation chose instead of
  deriving sockets from a constant (Task 3, Task 10 step 7). A substitution,
  not a change of algorithm.
- **Weighting springs by criticality.** Deferred until a measurement says plain
  wirelength misses the 15-cell constraint often enough to matter.
- **The optimiser.** `optimise` keeps its move set and stays off the shipping
  path. Whether local search has anything to contribute to a layout decided by
  physics is a question for after this lands.
- **Design H.** `Weld::BesideAt` and cell cohesion exist for it and have no
  caller. The DFF also needs `PlanCandidate` widened to one anchor per
  primitive -- the seam Task 9 works around rather than opens.
