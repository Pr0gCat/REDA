# Spring Placement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace "rows by logic depth, columns by barycentre" with a
continuous spring relaxation, legalised onto the lattice afterwards, and route
`compile()` through it.

**Architecture:** `primitive_graph::expand` already produces primitives and
typed edges. Two new stages sit between it and routing: `relax` solves a
quadratic spring system exactly for the current facings and the current anchor,
chooses each body's best facing among four, and projects the spacing rule as a
hard constraint -- repeating, with the anchor doubling, until what the springs
want and what is legal stop disagreeing. `snap` rounds the result onto the lattice and
collapses each gate's bodies back to the one anchor `PlanCandidate` has room
for. Routing, realisation and the four invariants are untouched.

**Tech Stack:** Rust 2021, no new dependencies. Existing `PrimitiveGraph`,
`physical::variants`, `World`, `PlanCandidate`, `route_every_net`,
`realise_and_verify`, and `./check.sh`.

**Spec:** `docs/superpowers/specs/2026-08-13-spring-placement.md`. Read it
before Task 1; this plan implements it and does not restate its reasoning.

**Measured, before any of this was committed.** Tasks 1 and 5-9 were
transcribed into the crate and run, then reverted. They compiled clean under
`-D warnings` on the first attempt, and relaxation converged on every circuit
tried:

| circuit | nodes | anchors occupy, today | relaxed | steps |
|---|---|---|---|---|
| and4 | 11 | 4,095 | **1,035** | 7 |
| full_adder | 25 | 10,143 | **3,465** | 9 |
| seven_segment | 88 | 24,973 (from a grid) | **8,475** | 14, 2.4 s release |

`snap` accepted all three, and two runs of each agree bit for bit. Those are
anchor bounding boxes, not block counts -- blocks need routing, and routing
needs the facings relaxation chose, which is why Stage 1 stops short of the
number this design exists to beat.

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
| `src/compile/mod.rs` | `place_nor_gate` / `place_merge_gate` / `place_primary_input` / `gate_footprint` take a facing; `CompiledCircuit` records what each gate was built as. Stage 1 adds one line to it, `pub mod relax;`. | 0, 1, 3 |
| `src/compile/equivalence.rs`, `world_partition.rs`, `routing_stats.rs` | Ask the recorded facing instead of assuming north. | 0 |
| `src/compile/topology.rs` | Footprint-area tables derived from `geometry` instead of tabulated. | 0 |
| `src/compile/relax/linear.rs` *(new)* | Dense Cholesky for one SPD system: factorise, then solve each axis against that factorisation. Pure numerics, no domain knowledge. | 1 |
| `src/compile/relax/build.rs` *(new)* | Bodies, pulls and welds from a netlist and its primitive graph. Where junctions get re-inserted. | 1 |
| `src/compile/relax/project.rs` *(new)* | The separation rule and the welds, alternating, welds last. Stage 2 adds nothing: `Axes::ALL` and the vertical requirement are built here in Stage 1 and switched on later. | 1 |
| `src/compile/relax/snap.rs` *(new)* | Rounding onto the lattice, refusing an unconverged placement, collapsing bodies to gate anchors. | 1 |
| `src/compile/relax/mod.rs` *(new)* | `relax` itself: the step loop, facing enumeration, convergence, errors. Stage 2 adds the `project_for_test` fixture module. | 1, 2 |
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

- [ ] **Step 1: Write the failing tests, and declare the module**

Create `src/compile/geometry.rs` containing only this test module for now, and
declare it in the same step -- beside the existing `pub mod physical;` at
`src/compile/mod.rs:58`:

```rust
pub mod geometry;
```

A `.rs` file no `mod` declaration names is not part of the crate, so rustc
never reads it: without the declaration Step 2 compiles clean, matches zero
tests and exits 0, which is not a red test but a test that did not run.

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
`output_direction` do not exist.

- [ ] **Step 3: Write the implementation**

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

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --release --lib compile::geometry`

Expected: 4 passed.

- [ ] **Step 5: Run the whole suite -- nothing else may move**

Run: `./check.sh`

Expected: `passed=470 failed=0 ignored=3` plus the four geometry tests, so
`passed=474`. Clippy clean, viewer green.

- [ ] **Step 6: Commit**

```bash
git add src/compile/geometry.rs src/compile/mod.rs && git commit -m "feat(geometry): a gate's facing is a thing that can be asked, not assumed"
```

---

### Task 2: A gate cell is built to a facing

**Files:**
- Modify: `src/compile/mod.rs` -- `place_nor_gate` (364-444), `place_merge_gate` (446-560), `place_primary_input` (~2476-2485), `gate_footprint` (6385-6459), and every caller
- Modify: `src/compile/planner.rs` -- `output_pin` (684-694), the seed loop's `gate_footprint` call (1888-1889), `emit_primitives`' two `place_*_gate` calls (613, 621) and its `place_primary_input` call (641)
- Modify: `src/compile/topology.rs` -- the footprint round-trip test's `place_nor_gate` / `place_merge_gate` calls (1799, 1811). Test code, and load-bearing: `check.sh` runs `cargo clippy --all-targets -- -D warnings`, so a call left at three arguments is a build failure like any other.
- Test: `src/compile/mod.rs` (in-file `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `geometry::{CellFacing, input_directions, output_direction, rotate}` from Task 1.
- Produces:
  - `pub fn place_nor_gate(world: &mut World, origin: (i32, i32, i32), input_count: usize, facing: CellFacing) -> NorCell`
  - `pub fn place_merge_gate(world: &mut World, origin: (i32, i32, i32), input_count: usize, facing: CellFacing) -> NorCell`
  - `pub(crate) fn place_primary_input(world: &mut World, home: Position, facing: CellFacing) -> (Position, Position)` -- it is `pub(crate)` today (mod.rs:2476) and stays so; only the parameter is new. Its one out-of-module caller, `planner.rs:641`, is in-crate.
  - `pub(crate) fn gate_footprint(origin: (i32, i32, i32), gate: &Gate, facing: CellFacing) -> (Vec<Anchor>, Vec<Anchor>, Anchor)`
  - `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` **survive this task**, unused by
    the four functions above but still read by `equivalence.rs`,
    `world_partition.rs`, `routing_stats.rs`, `planner.rs` and `mod.rs`'s own
    legacy emitter and test module. Task 3 converts those readers and deletes
    the constants there, once the last one is gone.

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

- [ ] **Step 6: Pass north at every call site**

Every caller passes `geometry::CellFacing::NORTH`:

| Site | Call |
|---|---|
| `mod.rs:3738-3742` | `emit`'s per-gate placement |
| `mod.rs:4413` | `cell_geometry_by_input_count` |
| `mod.rs:6393-6397` | `gate_footprint`'s own two calls |
| `mod.rs:6472-6473` | `legacy_primitive_nodes` -> `gate_footprint` |
| `mod.rs:2476` region | `place_primary_input`'s callers inside `mod.rs` |
| `planner.rs:613, 621` | `emit_primitives`' two `place_*_gate` calls |
| `planner.rs:641` | `emit_primitives`' `place_primary_input` call -- `let (lever, _) = compile::place_primary_input(&mut world, home);`, the one out-of-module caller the Interfaces block names |
| `planner.rs:1888-1889` | the seed loop's `gate_footprint` |
| `topology.rs:1799, 1811` | the footprint round-trip test |

Leave `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` (mod.rs:253-265) where they
are. Deleting them here would break `equivalence.rs:79`, `world_partition.rs:74`
and `routing_stats.rs:39` on their `use` lines -- an `E0432` on a `use` is a
tree that does not build, and a tree that does not build cannot report
`failed=0` at Step 9 nor keep any of the four pinned measurements. They are
still `pub(crate)` and still read, so nothing warns. Task 3 Step 7 deletes them
once its last reader is converted, and Task 3 Step 9's `git grep` is what
proves no site was missed.

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

`emit_primitives` passes `compile::geometry::CellFacing::NORTH` at all three of
its call sites for now -- `place_nor_gate`/`place_merge_gate` and `output_pin`
at 613 and 621, and `place_primary_input` at 641. Task 10 Step 8 is where it
starts passing `candidate.facing_of(index)` instead. Not Task 8: Task 8 only
touches `src/compile/relax/mod.rs`, and `emit_primitives` is `planner`'s.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::tests::a_turned`

Expected: 2 passed.

- [ ] **Step 9: Prove nothing moved**

Run: `./check.sh`

Expected: `failed=0`, and `the_hand_written_circuits_keep_their_measured_size`
still pinning 472 / 1,784 / 6,416 / 16,244. If any of those four numbers
changed, a call site was given a facing other than north. `INPUT_DIRECTIONS`
and `OUTPUT_DIRECTION` still exist and are still read; the `git grep` that
proves they are gone belongs to Task 3 Step 9, not here.

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
- Modify: `src/compile/geometry.rs` -- `gate_sockets`, added by Step 3. Created in Task 1 and edited here, which is why it is on this list rather than only in the Interfaces block.
- Modify: `src/compile/mod.rs` -- `CompiledCircuit` (the struct and both construction sites at 6327-6334 and 6353-6360), `bypass_source_start` (1162-1169), `emit`'s pin loop (3767-3786), `resolve_directed_dust_terminals` (~4353), `source_pin_position` (4423-4442), `merge_gate_body_owners` (~4909), the test module's `directed_dust_terminals_cover_a_real_verilog_and4_merge_output` (~7131), and the deletion of `INPUT_DIRECTIONS` / `OUTPUT_DIRECTION` (253-265)
- Modify: `src/compile/equivalence.rs` -- import (79), `verify_gate_structure` (400-482), `verify_merge_gate_structure` (490-588), `verify_lamp` (621-658)
- Modify: `src/compile/world_partition.rs` -- import (74), `check_gate_input_arity_agrees` (282-355), `resolve_node_position` (378-439)
- Modify: `src/compile/routing_stats.rs` -- import (39), `source_pin` (360-371)
- Modify: `src/compile/planner.rs` -- imports (4-6), `route_in_order`'s two socket derivations (2089-2100, 2131-2148)
- Test: `src/compile/mod.rs`

**Interfaces:**
- Consumes: `geometry::{CellFacing, input_directions, output_direction}`.
- Produces:
  - `CompiledCircuit` gains `pub gate_facings: Vec<CellFacing>`, indexed by gate
    index, one entry per `netlist.gates`.
  - `pub fn gate_sockets(origin: Position, arity: usize, facing: CellFacing) -> Vec<Position>`
    in `geometry` -- the sockets a gate at `origin` occupies, in declared input
    order. Every module above calls this instead of writing the offset loop a
    seventh time. `Position`, not `Anchor`: the body is `origin.offset(..)` and
    `Anchor` has no `offset`, both callers hold a `Position` already, and
    `geometry` sits below `planner`, where `Anchor` lives.

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

All but one of these has the gate index in hand already -- `verify_gate_structure`
and `verify_merge_gate_structure` take `g: usize`, `check_gate_input_arity_agrees`
enumerates. `verify_lamp` is the exception, and it is called out below.

`equivalence::verify_gate_structure` (457) and `verify_merge_gate_structure`
(556) -- take `facing: CellFacing` as a parameter, passed by the caller from
`compiled.gate_facings[g]`, and replace `INPUT_DIRECTIONS.iter()` with
`geometry::input_directions(facing).iter()`.

`equivalence::verify_lamp` (621-658) -- the exception on this list, and the one
place a facing has to be *found* rather than passed. Its only caller
(equivalence.rs:345) loops over declared output *names* and holds no gate
index, and inside, the producing gate is found by `.find(|gate| gate.output ==
output_name)`, which yields a `&Gate`. A `&Gate` cannot index
`compiled.gate_facings`. Widening the caller would mean giving every declared
output a gate index it does not have; the smaller change is to keep the search
where it is and have it return the index it already computed:

```rust
    let driving = netlist
        .gates
        .iter()
        .position(|gate| gate.output == output_name)
        .expect("every declared output is driven by a gate -- checked by `compile` before this ever runs");
    let &(tx, ty, tz) = compiled
        .gate_output_positions
        .get(&netlist.gates[driving].output)
        .ok_or_else(|| EquivalenceError::TorchNotPlaced {
            gate: netlist.gates[driving].output.clone(),
        })?;

    let expected = Position::new(tx, ty, tz)
        .offset(geometry::output_direction(compiled.gate_facings[driving]))
        .down();
```

`.position` rather than `.find`, and every later use of `driving_gate` becomes
`netlist.gates[driving]`. The `expect` is the one the code already carries,
unchanged and for the same reason.

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

- [ ] **Step 6: Make `mod.rs`'s own six sites ask**

`bypass_source_start`, `emit`'s pin loop, `resolve_directed_dust_terminals`,
`source_pin_position` and `merge_gate_body_owners` all sit inside the legacy
emitter, which builds every gate north. Each takes the facing from the gate
index it already has, via a local `let facing = geometry::CellFacing::NORTH;`
at the top of `emit` threaded through -- or, where the function has no gate
index, an added parameter. The mechanical rule: no call to
`geometry::input_directions` or `geometry::output_direction` may pass a literal
`CellFacing::NORTH` from inside a function that knows a gate index.

The sixth is in `mod.rs`'s own test module, and it is the one every inventory
missed: `directed_dust_terminals_cover_a_real_verilog_and4_merge_output`
(~7131) derives a NOR's sockets by hand inside a `flat_map` over the gates.
`check.sh` runs `cargo clippy --all-targets -- -D warnings`, so test code is
compiled and this site is as load-bearing as the other five. It already
enumerates the gates, so keep the index it currently discards and ask the
compiled circuit instead:

```rust
                (0..gate.inputs.len()).map(move |input| support.offset(INPUT_DIRECTIONS[input]))
```

becomes

```rust
                geometry::gate_sockets(support, gate.inputs.len(), compiled.gate_facings[g])
                    .into_iter()
```

with the enclosing `.flat_map(|(_, gate)| ...)` binding the gate index as `g`
rather than discarding it.

- [ ] **Step 7: Make `route_in_order` ask, then delete the constants**

`planner.rs:6` is `use crate::compile::{self, CompiledCircuit, LegacyEmission, Netlist};`,
and `{self, ..}` binds only the name `compile` -- a bare `geometry::` does not
resolve in that file. Widen it first, because everything below writes
`geometry::CellFacing`:

```rust
use crate::compile::{self, geometry, CompiledCircuit, LegacyEmission, Netlist};
```

`planner.rs:2089-2100` and `2131-2148` derive a socket from
`compile::INPUT_DIRECTIONS[input_index]`, which is about to stop existing. Both
become:

```rust
            let facing = candidate.facing_of(gate);
            let socket = step(support, compile::geometry::input_directions(facing)[input_index]);
```

with `PlanCandidate::facing_of(&self, node: usize) -> CellFacing` reading
`variant_indices` -- a field the struct already has, that every constructor
fills with zeroes and that nothing scores or branches on. It does have one
reader: `gate_efforts` (planner.rs:2967) copies it into `GateEffort::variant`,
a diagnostic that ships in `OptimisationReport`. `topology_alternatives` reads
only `gate` and `selected_entry`, and the cost term that once used the
orientation index -- `predicted_local_cost` -- is already deleted, so nothing
in the search sees it. Task 10 is where it stops being zero, and a turned gate
reporting a non-zero variant is that field finally doing what it was declared
for:

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
index. They can disagree today. Task 10 Step 9 is where that is settled, and it
needs a `RouteSink` threaded out of `route_endpoints` to do it; leave it alone
here, and leave a comment saying so.

With `route_in_order` converted, nothing names either constant any more:
`planner.rs`, `equivalence.rs`, `world_partition.rs` and `routing_stats.rs` are
Steps 5 and 7's, and `mod.rs`'s six sites are Step 6's. Now delete
`INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` (mod.rs:253-265). This is the task
that owns the deletion -- Task 2 left them alive precisely so that its own tree
would build -- and Step 9's `git grep` is what proves nothing was missed.

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
- Modify: `src/compile/topology.rs` -- module imports (44), `nor_footprint_area` (1274-1281), `merge_footprint_area` (1309-1315)
- Test: `src/compile/topology.rs` -- one new test, `footprint_area_does_not_depend_on_facing` (Step 1), plus the existing round-trip test at 1788-1818, which is not edited and must still pass. The new one says the derivation is a rotation; the old one says the derivation reproduces 6/9/12 and 6/9 against a really-placed cell. Neither claim is the other.

**Interfaces:**
- Consumes: `geometry::{CellFacing, gate_sockets, output_direction}` from Tasks 1
  and 3, and `redstone::simulator::position::Position`. `topology.rs` has
  exactly one module-level import today (`use std::collections::{BTreeMap, BTreeSet};`
  at line 44), so both of those are new lines beside it.
- Produces: no new public API. Both functions keep their signatures --
  including the `u32` return type five callers depend on -- and their answers.

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

`topology.rs` names neither `geometry` nor `Position` today, so both go in
beside its one existing import at line 44:

```rust
use crate::compile::geometry;
use crate::redstone::simulator::position::Position;
```

```rust
/// The ground-plan area a gate cell occupies, computed from where its cells
/// are rather than tabulated.
///
/// A cell is its origin, one socket per declared input, and its outbound pin
/// `pin_hops` out along its output face -- two for a NOR, whose torch stands
/// between origin and pin, one for a merge, whose junction *is* the origin.
///
/// `u32`, because that is what the two tabulated functions returned and what
/// `RealisationCost::area` (topology.rs:1365) is: five call sites accumulate
/// into a `u32` and would not compile against a `usize`.
fn footprint_area(arity: usize, facing: geometry::CellFacing, pin_hops: i32) -> u32 {
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

    ((max.0 - min.0 + 1) * (max.1 - min.1 + 1)) as u32
}

fn nor_footprint_area_facing(arity: usize, facing: geometry::CellFacing) -> u32 {
    assert!((1..=3).contains(&arity), "a NOR gate's fan-in is 1..=3, got {arity}");
    footprint_area(arity, facing, 2)
}

fn merge_footprint_area_facing(arity: usize, facing: geometry::CellFacing) -> u32 {
    assert!((2..=3).contains(&arity), "a wire-merge OR's fan-in is 2..=3, got {arity}");
    footprint_area(arity, facing, 1)
}

fn nor_footprint_area(arity: usize) -> u32 {
    nor_footprint_area_facing(arity, geometry::CellFacing::NORTH)
}

fn merge_footprint_area(arity: usize) -> u32 {
    merge_footprint_area_facing(arity, geometry::CellFacing::NORTH)
}
```

Keep both `nor_footprint_area` and `merge_footprint_area` at their existing
signatures -- `-> u32`, not `usize` -- so their callers are untouched:
topology.rs:1421 multiplies by `negative_inputs.len() as u32`, 1445/1449/1574
add into a `u32` accumulator, 1556 assigns straight into the `u32`
`RealisationCost::area`, and the round-trip tests at 1803 and 1815 compare
against `(x * z) as u32`. Delete the `match` tables and move their doc comments
-- the derivation prose at topology.rs:1283-1308 is right and is now what the
code does. The two `assert!`s replace the `unreachable!` arms those tables had:
a derivation happily computes an area for a fan-in no placer will build, and
the guard is the only thing that said so.

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

It answers it in *anchors*, not in blocks, and the difference is not a
technicality. and4's converged placement contains a WEST and an EAST facing,
full_adder's contains five -- so it cannot be built at all until Stage 0's
Tasks 2 and 3 have taught `place_nor_gate`, `gate_footprint` and the three
verifiers to honour a facing. That is why Stage 0 comes first. Task 10's
"572 blocks and 24 ticks" comparison is only meaningful once it does.

### Task 5: A linear solve with no dependency

**Files:**
- Create: `src/compile/relax/linear.rs`
- Create: `src/compile/relax/mod.rs` (declaring and re-exporting `linear`, for now)
- Modify: `src/compile/mod.rs` (add `pub mod relax;`)
- Test: `src/compile/relax/linear.rs`

**Interfaces:**
- Consumes: nothing. No imports outside `core`.
- Produces:
  - `pub struct Factorisation` with
    `Factorisation::of(matrix: &[f64], order: usize) -> Result<Factorisation, NotPositiveDefinite>`
    and `fn solve(&self, rhs: &mut [f64])`. No `order()` accessor: every caller
    tracks the order it built the matrix from, and an accessor nobody calls is
    a `dead_code` error under `-D warnings`.
  - `pub struct NotPositiveDefinite { pub row: usize }`

- [ ] **Step 1: Write the failing tests, and declare the modules**

Create `src/compile/relax/linear.rs` with the test module below. Declare the
modules in this same step, because an undeclared `.rs` file is not part of the
crate and rustc never reads it -- Step 2 would compile clean, match zero tests
and exit 0, which is a test that did not run rather than a test that failed.

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

// Re-exported rather than kept private: nothing outside `#[cfg(test)]` calls
// the solver until Task 8, and a `pub` item in a private module that nobody
// reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use linear::{Factorisation, NotPositiveDefinite};
```

Then `src/compile/relax/linear.rs`:

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

    /// The bare Laplacian of one edge: translation is free, so there is no
    /// unique answer -- and a solver that returns one anyway returns a
    /// placement nobody can reproduce.
    ///
    /// A matrix `relax` never builds, deliberately. Task 8 adds an anchor to
    /// every diagonal entry, which is what makes the system solvable with
    /// nothing pinned. This is the statement that the refusal is real, so that
    /// `RelaxError::Unsolvable` means what Task 8 says it means: not a
    /// component free to translate, but a graph built wrong.
    #[test]
    fn a_system_with_no_unique_answer_is_refused() {
        let error = Factorisation::of(&[1.0, -1.0, -1.0, 1.0], 2)
            .expect_err("a free translation has no unique answer");
        assert_eq!(error.row, 1);
    }

    /// Add an anchor to that same edge's diagonal and it becomes solvable, at
    /// every anchor strength down to the weakest one Task 8 uses.
    ///
    /// This is the property the whole step loop rests on: `A + λI` with
    /// `λ >= 1` is strictly diagonally dominant, so it is positive definite
    /// whether or not anything is pinned -- and `compile()` pins nothing.
    #[test]
    fn an_anchor_on_the_diagonal_makes_the_same_system_solvable() {
        for anchor in [1.0, 2.0, 1024.0] {
            let factorisation = Factorisation::of(&[1.0 + anchor, -1.0, -1.0, 1.0 + anchor], 2)
                .unwrap_or_else(|error| panic!("anchor {anchor} left row {} flat", error.row));
            // Both bodies anchored to the same place: the spring is already at
            // rest there, so that is where they stay.
            let mut rhs = [7.0 * anchor, 7.0 * anchor];
            factorisation.solve(&mut rhs);
            assert!((rhs[0] - 7.0).abs() < 1e-12, "anchor {anchor} landed at {}", rhs[0]);
            assert!((rhs[1] - 7.0).abs() < 1e-12, "anchor {anchor} landed at {}", rhs[1]);
        }
    }

    /// Striking a pinned body out works too, and is what the solve does with
    /// one: the free body lands exactly on the pinned one, because a spring at
    /// rest has zero length.
    #[test]
    fn striking_out_a_pinned_body_leaves_a_system_with_one_unknown() {
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

Expected: compile failure -- `Factorisation` and `NotPositiveDefinite` do not
exist.

- [ ] **Step 3: Write the implementation**

Prepend to `src/compile/relax/linear.rs`:

```rust
//! A dense Cholesky factorisation, and back-substitution against it.
//!
//! Deliberately small, and deliberately not a dependency. The crate has none
//! for linear algebra and compiles to wasm, where a foreign kernel's choice of
//! instruction is exactly the thing that would make native and browser layouts
//! disagree.
//!
//! One factorisation, then one solve per axis against it. The matrix is the
//! spring graph's weighted Laplacian with the pinned bodies struck out and the
//! step's anchor added to the diagonal; the graph and the stiffnesses hold
//! still for a whole relaxation, but the anchor doubles every step, so the
//! matrix is rebuilt and refactorised once per step and then serves all three
//! axes -- one `O(n^3/3)` against three `O(n^2)`. A sparse solver would buy
//! nothing until circuits are much larger than seven_segment's couple of
//! hundred bodies, and would cost the property this one has for free: the loop
//! order is fixed, nothing is parallel, and `f64` addition, multiplication and
//! `sqrt` are exact IEEE-754 operations, so two toolchains agree bit for bit.

/// A symmetric positive-definite matrix, factorised as `L * Lᵀ`.
///
/// `Debug` because `a_system_with_no_unique_answer_is_refused` calls
/// `expect_err`, which is bound `where T: Debug` on the `Ok` type.
#[derive(Debug)]
pub struct Factorisation {
    lower: Vec<f64>,
    order: usize,
}

/// Where the factorisation ran out of positive pivot.
///
/// For a bare Laplacian this means a connected component that may slide freely,
/// so the system has no unique answer. `relax` never hands one over: it adds an
/// anchor to every diagonal entry, which makes the matrix strictly diagonally
/// dominant whatever the graph looks like. So what reaches this type from there
/// is a stiffness that is not positive or a pull whose two ends are the same
/// body -- a graph built wrong, which is what `RelaxError::Unsolvable` says.
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
    ///
    /// Indexed rather than iterated on purpose: back-substitution reads
    /// `rhs[k]` for `k < i` while writing `rhs[i]`, and the fixed loop order is
    /// what makes two toolchains agree bit for bit. Clippy's
    /// `needless_range_loop` fires on both inner loops and its suggestion --
    /// iterate the slice -- is the one thing this must not do.
    #[allow(clippy::needless_range_loop)]
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
}
```

`order` stays a private field with no accessor. Every caller builds the matrix
and therefore already knows its order; an accessor nobody calls is `dead_code`,
which `check.sh` promotes to an error.

- [ ] **Step 4: Run the tests**

Run: `cargo test --release --lib compile::relax::linear`

Expected: 5 passed.

- [ ] **Step 5: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. Nothing outside `#[cfg(test)]` calls the solver until
Task 8, which is why `relax/mod.rs` re-exports it: a private `mod linear;`
would make `Factorisation`, `NotPositiveDefinite` and every associated item
unreachable, and `cargo clippy --all-targets -- -D warnings` reports that as
five errors, not five warnings.

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
- Consumes: `compile::geometry::CellFacing`, `compile::physical::{PortKind, RelativeSide, variants}`, `compile::primitive_graph::{PrimitiveGraph, NodeId, Provenance}`, `compile::topology::{Primitive, TemplateNode}`, `compile::{Netlist, Gate}`, `compile::planner::{Anchor, PortPlacements}`, `redstone::simulator::position::Position`, `redstone::world::block::BlockKind` -- the last two because `cells` steps offsets and reads a variant's block kinds. Not `PlannerError`: `build` is below `planner` in the dependency order and must not reach up into it.
- Produces:
  - `pub struct Body { pub what: BodyKind, pub position: [f64; 3], pub inputs: Vec<String>, pub output: Option<String>, pub facing: CellFacing, pub pinned: bool }` -- `inputs` and `output` are load-bearing, not decoration: `cells` branches on them, and Task 7's fixture constructs all six fields
  - `pub enum BodyKind { Primitive { node: NodeId, kind: Primitive }, Junction { gate: usize } }`
  - `pub enum Attach { Socket(usize), Pin, Port(PortKind) }`
  - `pub struct Pull { pub from: (usize, Attach), pub to: (usize, Attach), pub stiffness: f64 }`
  - `pub enum Weld { AtSocket { repeater: usize, junction: usize, input_index: usize }, BesideAt { lock: usize, data: usize, side: RelativeSide } }`
  - `pub struct BodyGraph { pub bodies: Vec<Body>, pub pulls: Vec<Pull>, pub welds: Vec<Weld>, pub nodes: Vec<Vec<usize>>, pub anchor_body: Vec<usize> }`
  - `pub fn attach_offset(attach: Attach, body: &Body) -> [f64; 3]`
  - `pub fn pin_hops(body: &Body) -> i32` -- 2 for a NOR's torch, 1 for
    everything else
  - `pub struct Cell { pub offset: (i32, i32, i32), pub carries: Vec<String> }`
    -- a *list*, because a NOR's support is on every input net at once; empty
    means inert
  - `pub fn cells(body: &Body) -> Vec<Cell>`
  - `pub fn build(netlist: &Netlist, graph: &PrimitiveGraph, start: &[Anchor], pinned: &PortPlacements) -> Result<BodyGraph, String>` -- the error is a sentence, because all four failures are one: "a gate instantiated no primitive", "a declared input has no lever", "this primitive has no physical variants" (Step 6), and "a gate declares more inputs than a gate cell has input faces" (Step 7). `RelaxError::CannotBuild` is what carries it
  - `pub const SIGNAL_STIFFNESS: f64 = 1.0;`

- [ ] **Step 1: Write the failing tests, and declare the module**

Create `src/compile/relax/build.rs` with the test module below, and add
`mod build;` to `src/compile/relax/mod.rs` in the same step -- an undeclared
file is not compiled, so Step 2 would report zero tests rather than a failure.
The re-exports wait for Step 8, when there is something to re-export.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    fn merge(output: &str, inputs: &[&str]) -> Gate {
        Gate::merge(output, inputs)
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

    /// A lever's pin is one hop out, which is what `place_primary_input`
    /// writes. Two is a NOR's answer, and only because its torch stands in the
    /// first hop; nothing stands in a lever's.
    ///
    /// Tested because an earlier draft keyed the hop count off `BodyKind`,
    /// where a lever and a NOR's torch are the same variant -- so every spring
    /// leaving a primary input attached one cell past the pin that exists, and
    /// no test looked.
    #[test]
    fn a_levers_pin_is_one_hop_out() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let lever = &graph.bodies[graph.anchor_body[1]];

        assert_eq!(attach_offset(Attach::Pin, lever), [0.0, 0.0, -1.0]);
        assert_eq!(
            cells(lever).len(),
            4,
            "a lever is its own cell and its pin, and `place_primary_input` \
             floors both"
        );
    }

    /// A lever is a power source, and its own block is that source. Marking it
    /// inert would let a foreign net run flush against it -- which is the
    /// 2026-08-12 failure exactly, one body over.
    #[test]
    fn a_levers_own_cell_is_on_the_net_it_drives() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let lever = &graph.bodies[graph.anchor_body[1]];

        let origin = cells(lever)
            .into_iter()
            .find(|cell| cell.offset == (0, 0, 0))
            .expect("a lever occupies its own cell");
        assert_eq!(origin.carries, vec!["a".to_string()], "a lever is not inert");
    }

    /// The rule the 2026-08-12 full adder broke, tested on the body it broke
    /// it on. That adder passed all four physical invariants and computed the
    /// wrong sums, because a foreign net was free to run against a support the
    /// code treated as inert.
    ///
    /// And the support is on *every* input net, not the first: a two-input
    /// NOR's second socket shares a net with it, so separation must not push
    /// them apart.
    #[test]
    fn a_nors_support_conducts_on_every_one_of_its_input_nets() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a", "b"])],
        };
        let graph = built(&netlist);
        let gate = &graph.bodies[graph.anchor_body[0]];
        let cells = cells(gate);

        let support = cells
            .iter()
            .find(|cell| cell.offset == (0, 0, 0))
            .expect("a NOR occupies its support");
        assert_eq!(
            support.carries,
            vec!["a".to_string(), "b".to_string()],
            "the support is the sink of both branches"
        );
    }

    /// A NOR's pin is two hops out because its torch stands in the first, and
    /// both cells are on the net it drives. An earlier draft keyed the hop
    /// count off `BodyKind`, which got this right and a lever's wrong.
    #[test]
    fn a_nors_torch_and_pin_are_two_hops_out_on_the_net_it_drives() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let gate = &graph.bodies[graph.anchor_body[0]];
        assert_eq!(pin_hops(gate), 2, "a torch stands in the first hop");

        let out = geometry::output_direction(gate.facing);
        let one = Position::new(0, 0, 0).offset(out);
        let two = one.offset(out);
        for step in [one, two] {
            let cell = cells(gate)
                .into_iter()
                .find(|cell| cell.offset == (step.x, step.y, step.z))
                .unwrap_or_else(|| panic!("a NOR occupies {step:?}"));
            assert_eq!(cell.carries, vec!["out".to_string()]);
        }

        let pin = attach_offset(Attach::Pin, gate);
        assert_eq!(pin, [two.x as f64, two.y as f64, two.z as f64]);
    }

    /// A junction's floor is inert -- `place_merge_gate` writes it, and
    /// nothing has to keep a net clear of it.
    #[test]
    fn a_junctions_floor_is_inert() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["m".into()],
            gates: vec![merge("m", &["a", "b"])],
        };
        let graph = built(&netlist);
        let junction = &graph.bodies[graph.anchor_body[0]];
        assert!(
            matches!(junction.what, BodyKind::Junction { .. }),
            "a merge is placed by a junction"
        );

        let floor = cells(junction)
            .into_iter()
            .find(|cell| cell.offset == (0, -1, 0))
            .expect("place_merge_gate floors its junction");
        assert!(floor.carries.is_empty(), "a floor keeps nothing out");
    }

    /// A gate cell has three input faces, and a merge is the one gate that can
    /// reach `build` asking for a fourth: `compile` admits it on
    /// `Or(4).accepts_arity(4)` and `expand`'s merge path never consults the
    /// library. Refused with a sentence, rather than panicking on a
    /// `[Facing; 3]` one stage before `place_merge_gate`'s own `assert!`.
    #[test]
    fn a_merge_wider_than_a_gate_cell_is_refused_rather_than_indexed() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            outputs: vec!["m".into()],
            gates: vec![merge("m", &["a", "b", "c", "d"])],
        };
        // `expand` really does let this through -- the refusal below is
        // `build`'s to make, not a restatement of one already made.
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; netlist.gates.len() + netlist.inputs.len()];

        let refusal = build(&netlist, &graph, &start, &PortPlacements::default())
            .expect_err("four inputs do not fit on three faces");
        assert!(
            refusal.contains('m') && refusal.contains('4'),
            "the refusal names the gate and its arity, got {refusal:?}"
        );
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
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockKind;

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
/// How many hops out along a body's output face its pin sits.
///
/// One for everything except a NOR, whose torch stands in the first hop and
/// whose pin is therefore the second: `place_merge_gate` puts a merge's pin
/// one hop from its junction, and `place_primary_input` puts a lever's one hop
/// from the lever. An earlier draft keyed this off [`BodyKind`], which made a
/// lever's pin two hops out here and one hop out everywhere it is really
/// written.
pub fn pin_hops(body: &Body) -> i32 {
    match body.what {
        BodyKind::Primitive { kind: Primitive::Torch, .. } => 2,
        _ => 1,
    }
}

/// Where an attachment sits relative to its body's own position.
///
/// A gate cell's origin is its support (a NOR) or its junction (a merge), and
/// both put their sockets on `geometry::input_directions` and their pin out
/// along `geometry::output_direction`, [`pin_hops`] of them.
pub fn attach_offset(attach: Attach, body: &Body) -> [f64; 3] {
    let facing = body.facing;
    match attach {
        Attach::Socket(index) => {
            let direction = geometry::input_directions(facing)[index];
            let step = Position::new(0, 0, 0).offset(direction);
            [step.x as f64, step.y as f64, step.z as f64]
        }
        Attach::Pin => {
            let direction = geometry::output_direction(facing);
            let mut step = Position::new(0, 0, 0);
            for _ in 0..pin_hops(body) {
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

`Attach::Socket(index)` panics for `index >= 3`, and `build` makes sure it is
never called that way: Step 7 refuses a gate with four or more declared inputs
with a sentence, before it builds any body for that gate.

That refusal is not belt and braces. `expand` rejects a four-input *NOR* --
`library.choose(Nor(4))` finds nothing, because `default_library` registers NOR
arities 1..=3 only -- but its merge path never consults the library at all, and
`Or(4).accepts_arity(4)` is true because `Or`'s arity is whatever it was
declared with, so a four-input merge reaches `build` intact. Without the check
it would index a `[Facing; 3]` and panic one stage before `place_merge_gate`'s
`assert!` says what the rule is.

- [ ] **Step 5: Write `cells` -- what a body occupies, and what each cell carries**

Separation is between **cells carrying different signals**, not between body
centres. A body is not a point: a torch is its support and its torch block, and
which of those two a foreign net may run against is the difference between a
legal circuit and a legal circuit that computes the wrong function. On
2026-08-12 the planner placed a full adder that passed all four invariants and
computed the wrong sum, because a foreign net was free to run against a NOR's
support -- which is the gate's input node -- that the code treated as inert.

```rust
/// One cell a body occupies, and every net that may lawfully touch it.
///
/// A list rather than one name, because a NOR's support is the sink of *all*
/// its input branches at once and each of them is allowed against it. An
/// earlier draft carried only the first, on the argument that what mattered
/// was being neither inert nor on the output net. It mattered more than that:
/// with one label, a two-input NOR's support says `a` while its second socket
/// says `b`, and separation then pushes `b`'s producer away from the very
/// socket the springs are pulling it onto -- on every gate of arity two or
/// more, for every input past the first.
///
/// Empty means inert: a repeater's floor, a junction's floor, a lever's two.
/// Nothing has to keep clear of it beyond cell exclusivity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub offset: (i32, i32, i32),
    pub carries: Vec<String>,
}

/// Every cell `body` occupies, and what each carries.
///
/// A gate cell's answer is `place_nor_gate`'s and `place_merge_gate`'s, stated
/// as signals rather than as blocks:
///
/// - the **support** (or junction) carries every declared input's net -- all
///   of them, not the first. It is the gate's input node: dust laid against it
///   powers it, and a NOR is N sources into one sink.
/// - each **socket** carries its own branch's net, and is a conductor even
///   though it is air: what ends up there is dust or a repeater.
/// - the **torch** and the **pin** carry the gate's own output net.
/// - a **floor** -- a repeater's, a junction's, a lever's -- carries nothing.
pub fn cells(body: &Body) -> Vec<Cell> {
    let facing = body.facing;
    let mut cells = Vec::new();

    match (&body.output, body.inputs.is_empty()) {
        // A body carrying a gate's output is a gate cell: support or junction,
        // sockets, torch, pin.
        (Some(output), _) => {
            // The support or junction is the gate's input node -- N sources
            // into one sink -- so every input net may lawfully touch it, and
            // all of them are named.
            //
            // A lever has no inputs and its own block still conducts, on the
            // net it drives. Labelling it inert would let a foreign net run
            // straight against a lever.
            let mut sink = body.inputs.clone();
            if sink.is_empty() {
                sink.push(output.clone());
            }
            cells.push(Cell {
                offset: (0, 0, 0),
                carries: sink,
            });

            for (index, signal) in body.inputs.iter().enumerate() {
                let step = Position::new(0, 0, 0).offset(geometry::input_directions(facing)[index]);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: vec![signal.clone()],
                });
            }

            let out = geometry::output_direction(facing);
            let mut step = Position::new(0, 0, 0);
            for _ in 0..pin_hops(body) {
                step = step.offset(out);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: vec![output.clone()],
                });
            }

            // What each placer actually lays underneath, which is not the same
            // for all three gate cells.
            match body.what {
                // `place_merge_gate` floors its own junction (`ensure_floor`
                // before the dust), and nothing else it writes.
                BodyKind::Junction { .. } => {
                    cells.push(Cell {
                        offset: (0, -1, 0),
                        carries: Vec::new(),
                    });
                }
                // `place_primary_input` floors the lever's home *and* its pin
                // -- both `ensure_floor` calls -- and `physical::variants`
                // declares the same `DOWN: Solid` on every lever variant. A
                // lever body takes this gate-cell arm and so never consults
                // `physical`, which is how both went missing.
                BodyKind::Primitive {
                    kind: Primitive::Lever,
                    ..
                } => {
                    let home_floor = Position::new(0, 0, 0).down();
                    let pin_floor = step.down();
                    cells.push(Cell {
                        offset: (home_floor.x, home_floor.y, home_floor.z),
                        carries: Vec::new(),
                    });
                    cells.push(Cell {
                        offset: (pin_floor.x, pin_floor.y, pin_floor.z),
                        carries: Vec::new(),
                    });
                }
                // A NOR floors nothing. `place_nor_gate` writes stone *at* the
                // support rather than beneath it, hangs the torch on that
                // support's wall (`wall_torch`, no floor), and leaves its pin's
                // floor to the route that reaches the cell -- exactly as
                // `place_merge_gate` leaves its own pin's.
                _ => {}
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
                        // A repeater's floor is inert: a net may run beside it.
                        BlockKind::Solid => Vec::new(),
                        // Everything else is the component itself, on the net
                        // it repeats. A primitive placed this way always has
                        // exactly one input -- an isolated branch's repeater --
                        // and an empty list here would mean "inert", which is
                        // the one thing this cell is not.
                        _ => vec![body
                            .inputs
                            .first()
                            .cloned()
                            .expect("a primitive placed without an output repeats an input")],
                    },
                });
            }
        }
    }
    cells
}
```

A lever is the one primitive body with an `output` and no `inputs`, so it takes
the gate-cell arm with an empty socket list: origin, its pin one hop out, and
the floor under each. That is exactly what `place_primary_input` writes -- one
`ensure_floor` on the home, one on the pin. Taking this arm is also why the
lever's own `physical` variant, which declares the same `DOWN: Solid`, is never
consulted for it, and why the two floors had to be spelled out here.

**Two exemptions the projection needs, both from this table.** Cells sharing a
signal are exempt: a producer's pin and its consumer's socket carry the same
net, and the route between them is what makes them one. Sharing, not matching
-- a support is on every one of its gate's input nets, so a socket carrying the
second input shares one with it. Cells with an empty `carries` are exempt from
everything except occupying the same cell as another body.

**What this table leaves out, and what Task 7 has to decide because of it.** A
gate pin's floor is not modelled, for a NOR or for a merge, because neither
placer writes it: `place_nor_gate` only reserves the cell in the bounding box it
returns, and `place_merge_gate` says outright that `emit` writes the dust there
-- so the route that reaches the pin is what lays the floor under it. A **bare**
branch's socket is the same case: both placers leave every socket empty for the
router to finish, and `emit`'s runs call `ensure_floor` on each cell they lay,
so that socket's dust and its floor arrive together and neither is the gate's.
An **isolated** branch's socket *is* covered, and only because that repeater is
a body in its own right, placed as a primitive, whose `physical` variant
declares `DOWN: Solid`. So the table is "what each placer actually lays
underneath", consistently -- and Task 7's separation rule has to say what
happens when two bodies' unmodelled pin floors would want the same cell.

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
    /// The precondition the three `is_empty()` guards in `build` exist for:
    /// `physical.rs` really does have a primitive with no variants.
    ///
    /// Named for what it checks. It does not call `build` -- no netlist can
    /// reach those guards today, because nothing in the library instantiates a
    /// comparator -- so calling it a refusal test would claim coverage it does
    /// not have. What it pins is that the guards are not dead reasoning.
    #[test]
    fn physical_really_has_a_primitive_with_no_variants() {
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
///
/// `start` is only a guess, and a port `pinned` names ignores it: a pinned
/// body starts at its pin. Nothing afterwards would put it there -- the solve
/// strikes pinned bodies out, `perturb` skips them, `separate` displaces their
/// neighbour instead -- so seeding it here is what makes `Body::pinned`'s
/// promise `build`'s guarantee rather than its caller's discipline.
/// `starting_layout` already agrees; `build`'s own tests deliberately do not.
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
        // A gate cell has three input faces -- the fourth is the output's --
        // and `geometry::input_directions` is a `[Facing; 3]` that every
        // socket lookup indexes by declared input index: here for a merge's
        // branch repeaters, and in `attach_offset` and `cells` for every gate.
        // `place_nor_gate` and `place_merge_gate` each `assert!` the same
        // bound, but one stage later, so without this the index panic gets
        // there first with no gate name on it.
        //
        // Reachable, and only for a merge: `compile` admits a gate on
        // `is_realisable() && accepts_arity(len)`, and `Or(4).accepts_arity(4)`
        // is true because `Or`'s arity is whatever it was declared with.
        // `expand`'s merge path then never consults the library, so nothing
        // between here and there objects. A `Nor(4)` is stopped earlier --
        // `expand` asks `library.choose` for an entry and `default_library`
        // registers NOR arities 1..=3 only.
        let faces = geometry::input_directions(CellFacing::NORTH).len();
        if gate.inputs.len() > faces {
            return Err(format!(
                "gate `{}` declares {} inputs, and a gate cell has only {faces} input faces",
                gate.output,
                gate.inputs.len()
            ));
        }

        // A pin is where the body *is*, not merely a flag on it.
        let fixed = pinned.get(&gate.output);
        let at = fixed.unwrap_or(start[gate_index]);
        let position = [at.x as f64, at.y as f64, at.z as f64];
        let is_pinned = fixed.is_some();

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
            let node = *graph.gate_nodes[gate_index]
                .first()
                .ok_or_else(|| format!("gate `{}` instantiated no primitive", gate.output))?;
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
                let kind = graph.nodes[node].primitive;
                if physical::variants(kind).is_empty() {
                    return Err(format!(
                        "gate `{}`'s branch {input_index} needs a `{kind:?}`, which has no physical variants",
                        gate.output
                    ));
                }
                let direction = geometry::input_directions(CellFacing::NORTH)[input_index];
                let socket = Position::new(at.x, at.y, at.z).offset(direction);
                bodies.push(Body {
                    what: BodyKind::Primitive { node, kind },
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
        let fixed = pinned.get(name);
        let at = fixed.unwrap_or(start[candidate_node]);
        let kind = graph.nodes[node].primitive;
        if physical::variants(kind).is_empty() {
            return Err(format!(
                "declared input `{name}` needs a `{kind:?}`, which has no physical variants"
            ));
        }
        bodies.push(Body {
            what: BodyKind::Primitive { node, kind },
            position: [at.x as f64, at.y as f64, at.z as f64],
            // A lever drives its own name and reads nothing, which is what
            // gives it a gate cell's shape with no sockets.
            inputs: Vec::new(),
            output: Some(name.clone()),
            facing: CellFacing::NORTH,
            pinned: fixed.is_some(),
        });
        nodes[candidate_node].push(bodies.len() - 1);
        anchor_body[candidate_node] = bodies.len() - 1;
    }

    let pulls = signal_pulls(netlist, &anchor_body, &welds);

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
fn signal_pulls(netlist: &Netlist, anchor_body: &[usize], welds: &[Weld]) -> Vec<Pull> {
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

            // A branch with a welded repeater pulls on the repeater rather
            // than on the junction; the weld, not a spring, is what puts the
            // repeater in the socket.
            //
            // Not its rear, despite the port's name: `physical.rs` declares
            // every repeater port at `ORIGIN` and distinguishes them by
            // `direction`, which `attach_offset` drops -- because a repeater
            // occupies one cell. So this changes which body absorbs the force,
            // not where on that body the force lands.
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
    pulls
}
```

- [ ] **Step 8: Re-export the types**

`mod build;` landed in Step 1; this is the re-export. `src/compile/relax/mod.rs`
in full, so nothing is dropped by accident -- the module doc Task 5 Step 1 wrote
is part of "in full", and `linear`'s re-export from Task 5 is still what keeps
the solver out of `dead_code`:

```rust
//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod build;
mod linear;

// Re-exported rather than kept private: nothing outside `#[cfg(test)]` calls
// the model or the solver until Tasks 7 and 8, and a `pub` item in a private
// module that nobody reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use build::{
    attach_offset, build, cells, pin_hops, Attach, Body, BodyGraph, BodyKind, Cell, Pull, Weld,
    SIGNAL_STIFFNESS,
};
pub use linear::{Factorisation, NotPositiveDefinite};
```

Everything this task's Interfaces block produces, not just the types the next
task consumes. `mod build;` is private, so an item nobody re-exports is
reachable only from `#[cfg(test)]` code -- and `cargo clippy --all-targets`
compiles the lib target without `cfg(test)`, where that reads as `dead_code`
and `-D warnings` reads as an error. `build`, `cells` and `Cell` have no
non-test caller until Tasks 7 and 8; the re-export is what carries them across.
(`pin_hops` and `signal_pulls` are called by `attach_offset` and `build`, so
they are live either way -- listing `pin_hops` keeps the export and the
Interfaces block saying the same thing.)

- [ ] **Step 9: Run the tests**

Run: `cargo test --release --lib compile::relax::build`

Expected: 12 passed.

- [ ] **Step 10: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. Nothing calls `build` outside its own tests yet, which is
what Step 8's re-export is for -- clippy would otherwise call the whole module
dead.

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
  - `pub const PROJECTION_ROUNDS: usize = 4096;`
  - `pub const SETTLED: f64 = 1e-6;`
  - `pub fn reservation(routed_degree: usize) -> f64`
  - `pub struct Violation { pub left: usize, pub right: usize, pub shortfall: f64 }`
  - `pub struct Axes(&'static [usize]);` with `Axes::IN_PLANE` (`&[0, 2]`) and `Axes::ALL` (`&[0, 1, 2]`)
  - `pub fn project(graph: &mut BodyGraph, required: &[f64], axes: Axes) -> Result<(), Violation>`
  - `pub fn worst_violation(graph: &BodyGraph, required: &[f64]) -> Option<Violation>`
  - `pub fn required_separations(graph: &BodyGraph) -> Vec<f64>`
  - `pub struct PlacedCell { pub at: [f64; 3], pub carries: Vec<String> }`
  - `pub fn placed_cells(graph: &BodyGraph) -> Vec<Vec<PlacedCell>>`
  - Not produced: `Offence`, `offence`, `unseparated`, `cheapest_axis`,
    `welded_partners`, `exempt`, `separate` and `satisfy` are private to
    `project.rs`. `separate` in particular takes the axis and the distance it is
    told to move -- it chooses neither -- so there is no signature here for a
    caller to hold on to.

- [ ] **Step 1: Write the failing tests, and declare the module**

Create `src/compile/relax/project.rs` with the test module below, and add
`mod project;` to `src/compile/relax/mod.rs` in the same step -- an undeclared
file is never compiled, so Step 2 would report zero tests instead of a
failure. The re-exports wait for Step 7.

There is no `RelativeSide` import in this module. Every weld built below is a
`Weld::AtSocket`; `Weld::BesideAt`, the only variant carrying a side, has no
caller until Design H. `check.sh` runs
`cargo clippy --all-targets -- -D warnings`, so an import nothing uses is a
build failure rather than a tidy-up.

```rust
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
```

- [ ] **Step 4: Write the separation predicate and the required table**

```rust
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
```

- [ ] **Step 5: Write the weld exemption and the worst violation**

```rust
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
```

The pair's requirement is `max`, not the sum: a reservation is a ring around
one body, and two rings that overlap are still one corridor. Taking the sum
would charge a low-degree neighbour for its neighbour's fan-out.

The exemption table is built per pass rather than per pair for a reason worth
stating as a number. The pair loop is quadratic in the bodies, and rescanning
`welds` inside it made the exemption test the product of the two -- and the
whole of it runs [`PROJECTION_ROUNDS`] times on the deadlock path, which Task 8
puts inside an alternation. Measured on 2026-08-13 on 87 bodies with 58 welds,
driven to the full 4096 rounds: 767ms with the per-pair scan and the per-axis
re-walk of Step 4, 234ms with both hoisted, and the layout that comes out is
bit-identical.

- [ ] **Step 6: Write the projection**

```rust
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
```

- [ ] **Step 7: Re-export**

`mod project;` landed in Step 1. `src/compile/relax/mod.rs` now carries:

```rust
pub use project::{
    placed_cells, project, required_separations, reservation, worst_violation, Axes, PlacedCell,
    Violation, CONDUCTOR_CLEARANCE, PROJECTION_ROUNDS, ROUTE_PITCH, SETTLED, SNAP_MARGIN,
};
```

`SETTLED` is on that list for the same reason the rest are, and it is the one
with no reader at all outside `project.rs` -- `snap` spends `SNAP_MARGIN` and
`relax` compares against `CONVERGED`, so a constant that is `pub` in a private
module and named nowhere else is exactly the `dead_code` the re-export exists to
prevent.

Everything the Interfaces block produces, for the reason Task 6 Step 8 gives:
`mod project;` is private, `project`, `worst_violation` and
`required_separations` have no caller outside `#[cfg(test)]` until Task 8, and
an unreachable `pub` item is `dead_code` on the lib target -- an error under
`cargo clippy --all-targets -- -D warnings`, which Step 9 runs.

- [ ] **Step 8: Run the tests**

Run: `cargo test --release --lib compile::relax::project`

Expected: 7 passed.

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

The `c` is load-bearing and is where an earlier draft of this design failed: it
is an **anchor**, a pull from every body toward where the projection last
legally put it, and it doubles each step up to a ceiling. Without it the solve
collapses every free body onto its neighbours -- springs have zero rest length
-- and the projection unpicks the same knot for ever. With it, the solve is the
lower bound and the projection is the upper bound. Everything that leaves the
loop is a projected configuration, so it is legal at every step by construction.
The spec derives all of this; `ANCHOR_STIFFNESS` and `ANCHOR_GROWTH` in Step 3
are its two numbers, and `ANCHOR_CEILING` is where the doubling stops before an
`f64` overflows to `+inf` and the loop starts reporting `NaN`s as converged.

**Convergence is three conditions, not one.** The two bounds meeting -- `gap`,
the within-step disagreement between the solve and the projection -- is
necessary and nowhere near sufficient: `project` returns having moved nothing
whenever no pair is violating, so a slack constraint set makes `gap` exactly 0.0
however far the solve just carried the bodies. So the step's own displacement of
the legal configuration, `settled`, is conjoined to it, and that is the settling
test `ContinuousPlacement::converged` describes. `!turned` is the third, and it
is there so `gap` compares like with like: `solved` is the spring optimum for
the facings the solve ran under, and a turn changes the offsets both sides are
measured against. Step 6's comment carries the measurements.

Holding facings aside is what makes it linear. Choosing them is a
one-dimensional question with four answers, so it is an enumeration rather than
a rotation integrated over time.

**Files:**
- Modify: `src/compile/relax/mod.rs`
- Test: `src/compile/relax/mod.rs`

**Interfaces:**
- Consumes: `linear::Factorisation`, `build::{build, BodyGraph, attach_offset}`, `project::{project, required_separations, worst_violation, Axes, Violation}`, `geometry::CellFacing`.
- Produces:
  - `pub struct RelaxEffort { pub iterations: usize, pub seed: u64 }` with `Default` (`iterations: 256, seed: 0`)
  - `pub struct ContinuousPlacement { pub graph: BodyGraph, pub converged: bool, pub iterations: usize }`
  - `pub enum RelaxError { DidNotConverge { iterations: usize, worst: Violation }, Deadlocked { worst: Violation }, Unsolvable { component_row: usize }, CannotBuild { reason: String } }` with `Display`.
    Four, not three: `CannotBuild` is what carries `build`'s sentence (Task 6
    Interfaces), and Step 6 is the only thing that constructs it. Task 9 adds a
    fifth, `SurvivedSnap`, and Task 10 wraps the whole enum in
    `PlannerError::Relaxation` -- so a variant missing from this list is a
    variant missing from the error the planner reports.
  - `pub const CONVERGED: f64 = 0.1;`
  - `pub const ANCHOR_STIFFNESS: f64 = 1.0;`
  - `pub const ANCHOR_GROWTH: f64 = 2.0;`
  - `pub const ANCHOR_CEILING: f64 = (1u64 << 60) as f64;`
  - `pub fn relax(netlist: &Netlist, graph: &PrimitiveGraph, start: &[Anchor], pinned: &PortPlacements, axes: Axes, effort: RelaxEffort) -> Result<ContinuousPlacement, RelaxError>`

- [ ] **Step 1: Write the failing tests**

Add to `src/compile/relax/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::Library;
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

    /// Nothing has to be pinned, and on the netlist-only path nothing is: the
    /// viewer calls `compile_planned(&netlist, &PortPlacements::default())`
    /// (`viewer/src/lib.rs:862`) and `plan_from_netlist`'s own tests do the
    /// same. `compile()` is not that caller today -- it seeds the planner from
    /// the legacy emitter -- and Task 13 is where it becomes one.
    ///
    /// Without the anchor this is the system the solver refuses before it
    /// starts: every component is free to translate, the Laplacian is singular,
    /// and `Factorisation::of` returns `NotPositiveDefinite` on the first flat
    /// pivot. With `ANCHOR_STIFFNESS` on the diagonal it is strictly diagonally
    /// dominant, so it factorises whatever the graph looks like -- which is why
    /// there is no longer any mechanism that goes looking for a component to
    /// hold still.
    #[test]
    fn a_netlist_with_nothing_pinned_still_relaxes() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("the anchor is what makes an unpinned system solvable");
        assert!(placement.converged, "it stopped without the two bounds meeting");
    }

    /// A relaxation that ran out of iterations says so rather than handing
    /// back something that looks placed, and says how many it was given.
    ///
    /// `worst` is wildcarded here on purpose, unlike in `snap`'s counterpart --
    /// which is also why this test is not named for the pair. `relax` reaches
    /// that line only after a projection that returned `Ok`, which means it
    /// left no violation, so `worst_violation` is `None` and the field is the
    /// placeholder. The pair worth naming is the one `snap` finds on an
    /// unconverged placement, and that is where the spec asks for it.
    #[test]
    fn running_out_of_iterations_is_an_error_that_says_how_many_it_had() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        // Every body on one anchor, so one step cannot possibly finish: the
        // solve collapses them, the projection pulls them apart by at least
        // two cells, and the gap between those two answers is what convergence
        // is measured on.
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

    /// A slack constraint set is not a settled relaxation, and `gap` alone
    /// cannot tell them apart.
    ///
    /// This chain is laid out north-to-south with sixty cells between
    /// neighbours, so no pair is ever inside its separation requirement:
    /// `project` returns having moved nothing on every round, `legal` is
    /// bit-identical to `solved`, and `gap` is 0.0000 at *every* step. So the
    /// old two-condition test reduced here to `!turned` alone -- and `build`
    /// seeds every body [`crate::compile::geometry::CellFacing::NORTH`], which
    /// this layout makes the argmin from the first sweep on. It returned at
    /// step 2, two solves in, reporting `converged: true`.
    ///
    /// `settled` is what makes the loop keep going, and the numbers say by how
    /// much. Measured 2026-08-14: without it, 2 steps and 30.09 cells between
    /// `b` and `c`; with it, 9 steps and 24.78. Both bounds below are loose
    /// enough to leave that whole gap and still fail on the wrong side of it.
    #[test]
    fn a_slack_constraint_set_is_not_a_settled_one() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![
            Anchor { x: 0, y: 1, z: -60 },  // gate b
            Anchor { x: 0, y: 1, z: -120 }, // gate c
            Anchor { x: 0, y: 1, z: 0 },    // input a
        ];
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &placements,
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("a chain nothing crowds relaxes");

        assert!(
            placement.iterations >= 5,
            "it stopped after {} step(s): nothing was measuring whether it had settled",
            placement.iterations
        );
        let b = placement.graph.bodies[placement.graph.anchor_body[0]].position[2];
        let c = placement.graph.bodies[placement.graph.anchor_body[1]].position[2];
        assert!(
            (b - c).abs() < 27.0,
            "b and c are {:.3} apart; the springs had not finished pulling them together",
            (b - c).abs()
        );
    }

    /// A guardrail on the anchor schedule, not a golden number.
    ///
    /// [`ANCHOR_GROWTH`]'s doc argues that raising either anchor number
    /// converges sooner and places worse, and until this test that argument had
    /// nothing holding it: `ANCHOR_STIFFNESS = 1024.0` or `ANCHOR_GROWTH = 64.0`
    /// -- the corners the doc calls "converges in two steps and has placed
    /// nothing" -- passed every other assertion in the module.
    ///
    /// The ceiling is deliberately loose. and4 measures 1,125 at the shipped
    /// constants (2026-08-14, metric as [`ANCHOR_GROWTH`] states it), 2,028 at
    /// `g = 64`, 4,221 at `k = 1024` -- and 4,221 is also the starting layout's
    /// own box, so the `k = 1024` corner really has placed nothing. Anything
    /// under 1,500 is the shipped corner with room to move; an ordinary
    /// refactor that shifts a body or two will not trip it, and neither wrong
    /// anchor can get under it. The step floor catches `k = 1024` (2 steps)
    /// but *not* `g = 64`, which lands on exactly 3 -- the area assertion is
    /// what catches that corner.
    #[test]
    fn the_anchor_schedule_still_places_and4_small() {
        let (netlist, _) = crate::circuits::and4::build_and4_netlist();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> =
            crate::compile::planner::plan_from_netlist(&netlist, &PortPlacements::default())
                .expect("plans")
                .anchors()
                .to_vec();

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("and4 relaxes");

        let cells = placed_cells(&placement.graph);
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for body in &cells {
            for cell in body {
                for axis in [0usize, 2] {
                    lo[axis] = lo[axis].min(cell.at[axis]);
                    hi[axis] = hi[axis].max(cell.at[axis]);
                }
            }
        }
        let width = (hi[0].round() - lo[0].round()) as i64 + 1;
        let depth = (hi[2].round() - lo[2].round()) as i64 + 1;
        assert!(
            width * depth < 1_500,
            "and4 came back {width} by {depth} = {}; the anchor schedule has stopped placing",
            width * depth
        );
        assert!(
            placement.iterations >= 3,
            "and4 converged in {} step(s), which is what an anchor too strong to move anything \
             looks like",
            placement.iterations
        );
    }

    /// [`Axes`] governs the solve as well as the projection.
    ///
    /// Gate `b` starts a storey above everything that pulls on it, and in-plane
    /// relaxation has to leave it there. Replace the solve's `axes.iter()` with
    /// `0..3` and this is the only test in the module that notices: every other
    /// fixture starts every body at `y = 1`, so the Y right-hand side is
    /// `anchor * 1.0` for every free body and the Y solve returns exactly the
    /// storey it was given. Here the zero-rest-length Y springs pull `b` down
    /// onto its neighbours' plane instead -- and `gap` cannot see it, because
    /// the in-plane projection never writes Y, so `solved` and `legal` agree on
    /// that axis bit for bit however wrong it is.
    ///
    /// Exact equality rather than a tolerance: under [`Axes::IN_PLANE`] nothing
    /// writes `position[1]` at all -- `separate` moves only the axis
    /// `cheapest_axis` chose, and `chain()` has no welds, so `satisfy` never
    /// runs -- so the value has to be the starting one bit for bit.
    #[test]
    fn a_body_stays_on_the_storey_it_started_on() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![
            Anchor { x: 0, y: 6, z: 0 },   // gate b, one storey up
            Anchor { x: 40, y: 1, z: 0 },  // gate c
            Anchor { x: -20, y: 1, z: 0 }, // input a
        ];

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        for (node, anchor) in start.iter().enumerate() {
            let body = placement.graph.anchor_body[node];
            assert_eq!(
                placement.graph.bodies[body].position[1],
                f64::from(anchor.y),
                "node {node} left the storey its starting layout gave it"
            );
        }
    }

    /// The seed's one job, asserted. Seed zero changes nothing; a non-zero seed
    /// moves every unpinned body in plane and no pinned body at all, by less
    /// than the quarter cell that would carry one past a neighbour; and two
    /// seeds give two starts. Without this the whole of `perturb` could be
    /// emptied to its signature and the suite would not notice -- the only
    /// non-zero seed anywhere compares two runs at the *same* seed.
    #[test]
    fn a_seed_nudges_the_unpinned_in_plane_and_nothing_else() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);
        let built = || build::build(&netlist, &graph, &start, &placements).expect("builds");

        let mut untouched = built();
        perturb(&mut untouched, 0);
        for (index, (before, after)) in built().bodies.iter().zip(&untouched.bodies).enumerate() {
            assert_eq!(
                before.position.map(f64::to_bits),
                after.position.map(f64::to_bits),
                "seed zero moved body {index}"
            );
        }

        let mut nudged = built();
        perturb(&mut nudged, 0x26_02);
        for (index, (before, after)) in built().bodies.iter().zip(&nudged.bodies).enumerate() {
            assert_eq!(
                before.position[1], after.position[1],
                "body {index} left its storey"
            );
            if before.pinned {
                assert_eq!(
                    before.position.map(f64::to_bits),
                    after.position.map(f64::to_bits),
                    "pinned body {index} left its pin"
                );
                continue;
            }
            for axis in [0usize, 2] {
                let moved = (after.position[axis] - before.position[axis]).abs();
                assert!(moved > 0.0, "body {index} did not move on axis {axis}");
                assert!(moved < 0.25, "body {index} moved {moved} on axis {axis}");
            }
        }

        let mut other = built();
        perturb(&mut other, 0x26_03);
        assert_ne!(
            nudged.bodies[0].position.map(f64::to_bits),
            other.bodies[0].position.map(f64::to_bits),
            "two seeds have to give two starts"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::relax::tests`

Expected: compile failure -- `relax`, `RelaxEffort`, `ContinuousPlacement` do
not exist.

- [ ] **Step 3: Write the errors and the effort**

Add to `src/compile/relax/mod.rs`. The head of the file is Task 6 Step 8's,
module doc and re-export list included, and is repeated here for the same reason
it was repeated there -- so that "in full" cannot be read as "delete what is not
shown". The one edit this task makes to it is the re-export comment, which named
"Tasks 7 and 8" as where the readers arrive and now names 9 and 10, because
nothing outside `src/compile/relax/` names the module at this commit either:

```rust
//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod build;
mod linear;
mod project;

// Re-exported rather than kept private: `relax` below reaches some of these,
// and nothing outside this directory names the module at all until `snap` and
// the planner do, in Tasks 9 and 10. A `pub` item in a private module that
// nobody reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use build::{
    attach_offset, build, cells, pin_hops, Attach, Body, BodyGraph, BodyKind, Cell, Pull, Weld,
    SIGNAL_STIFFNESS,
};
pub use linear::{Factorisation, NotPositiveDefinite};
pub use project::{
    placed_cells, project, required_separations, reservation, worst_violation, Axes, PlacedCell,
    Violation, CONDUCTOR_CLEARANCE, PROJECTION_ROUNDS, ROUTE_PITCH, SETTLED, SNAP_MARGIN,
};

use crate::compile::planner::{Anchor, PortPlacements};
use crate::compile::primitive_graph::PrimitiveGraph;
use crate::compile::Netlist;

/// How far a body may still be moving and the relaxation still be finished.
///
/// A tenth of a cell, because the rounding margin is a whole one: a system
/// still twitching below that cannot change what `snap` produces, and running
/// past it buys nothing measurable.
pub const CONVERGED: f64 = 0.1;

/// How hard a body is pulled toward where it was last legally placed.
///
/// This is the `c` of the `Ax = f + c` the founding spec cites, and dropping
/// it is why an earlier draft of this design did not converge. An exact solve
/// has zero-rest-length springs collapse every free body onto its neighbours,
/// so the projection unpicks the same knot every step and the two take turns
/// undoing each other. Measured on 2026-08-13: `and4` deadlocked with two
/// bodies 0.030 too close and `full_adder` with two 1.372, from the starting
/// layout and from a naive grid alike -- while the projection *alone*
/// converged from both.
///
/// One, because that is one more spring of the same `k = 1` every signal
/// spring has: the weakest anchor that is not no anchor.
pub const ANCHOR_STIFFNESS: f64 = 1.0;

/// What the anchor is multiplied by after each step.
///
/// Doubling, so the anchor overwhelms a bounded degree in a number of steps
/// logarithmic in it.
///
/// **Raising either anchor number converges sooner and places worse.** An
/// earlier draft of this comment claimed the opposite -- that the schedule
/// decides only how many steps are spent, not what is found -- and a parameter
/// sweep on 2026-08-13 says otherwise. The projection is not onto a convex
/// set, so what the loop finds is a local optimum of how far the springs were
/// let run before the anchor clamped them.
///
/// **Area here is one stated metric**, not a word: round every cell centre of
/// [`placed_cells`] to the nearest lattice column, count the columns spanned on
/// X and on Z inclusive, and multiply. Both reference circuits are started from
/// `plan_from_netlist(netlist, &PortPlacements::default())`'s anchors with
/// `Axes::IN_PLANE`, nothing pinned, and `RelaxEffort::default()`. Re-measured
/// on 2026-08-14, after `settled` joined the convergence test -- an earlier
/// draft of this table named a metric it did not state and could not be
/// reproduced from the tree.
///
/// | | and4 area | full_adder area |
/// |---|---|---|
/// | `k = 1`, `g = 2` | **1,125** (45x25) in 8 steps | **3,638** (34x107) in 9 steps |
/// | `k = 4`, `g = 2` | 2,610 (58x45) in 7 | 7,140 (51x140) in 8 |
/// | `k = 1024`, `g = 2` | 4,221 (63x67) in 2 | 10,595 (65x163) in 2 |
/// | `k = 1`, `g = 64` | 2,028 (52x39) in 3 | 5,980 (46x130) in 3 |
///
/// At `k = 1024` what comes back is the layout that went in. The starting
/// layout measures 63x67 for and4 and 64x163 for full_adder in that same
/// metric, so and4's 63x67 is its input to the column and full_adder's 65x163
/// is one column wider on X -- the facing sweep still runs. The anchor pins the
/// solve to `x_legal` on the first step and the loop terminates on what it was
/// handed. So the temptation this comment exists to refuse is the obvious one:
/// a circuit is slow, raise the anchor, it converges in two steps and has
/// placed nothing.
///
/// `k = 1, g = 2` is the best-quality corner of that sweep and already
/// converges in single-digit steps. Raising `RelaxEffort::iterations` instead
/// costs nothing and changes nothing: at every corner above, budgets of 256,
/// 1024, 4096 and 16384 converge at the same step with the same box. That is
/// safe at any budget only because [`ANCHOR_CEILING`] stops the doubling --
/// without it a budget past 1024 is where the anchor overflows to `+inf`.
pub const ANCHOR_GROWTH: f64 = 2.0;

/// Where the doubling stops.
///
/// A cap rather than a documented bound, because the failure it prevents is
/// silent. `anchor` is an `f64` doubling from [`ANCHOR_STIFFNESS`], so it is
/// `+inf` from step 1025 -- and `+inf` is absorbed rather than refused at every
/// stage after that. [`Factorisation::of`] rejects only a pivot `<= 0.0`, and
/// `inf` is not; every pivot becomes `sqrt(inf) = inf`; the back-substitution
/// divides `inf` by `inf` and every free body's position becomes `NaN`. `NaN`
/// then passes each of `relax`'s three exit conditions rather than tripping
/// them: `choose_facings` never beats its `f64::INFINITY` seed because
/// `NaN < INFINITY` is false, so nothing turns; `project` finds no violation
/// because every comparison against `NaN` is false; and `fold(0.0, f64::max)`
/// ignores `NaN` by contract, so `gap` and `settled` both fold to 0.0. The call
/// would return `Ok` with `converged: true` and a placement of `NaN`s.
///
/// Reachable only through [`RelaxEffort::iterations`], which is a `pub` field
/// this file's own comments tell the reader to raise. The default of 256 does
/// not get near it.
///
/// Nothing measured here is lost by stopping. The anchor reaches `2^60` at step
/// 61, and both reference circuits converge in single digits: the budget sweep
/// in [`ANCHOR_GROWTH`] was re-run at 256, 1024, 4096 and 16384 with the cap in
/// place and every corner gave the same step and the same box. Past that point
/// the cap is not a compromise either -- once the anchor is `2^53` times a
/// body's incident stiffness the solve's answer differs from `legal` by less
/// than an ULP, so doubling further changes nothing but the exponent. A power
/// of two, so the multiply and the comparison stay exact.
pub const ANCHOR_CEILING: f64 = (1u64 << 60) as f64;

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
    /// Whether the last step met all three of [`relax`]'s exit conditions: the
    /// solve and the projection agreed to within [`CONVERGED`], the step moved
    /// the legal configuration less than [`CONVERGED`], and no body turned.
    ///
    /// [`relax`] returns this `true` or does not return at all -- a run that
    /// does not converge is [`RelaxError::DidNotConverge`], never an `Ok`
    /// carrying `false`. The field is here for the consumer, not the producer:
    /// `snap`, in Task 9, will refuse an unconverged placement, because
    /// rounding is exact only if the projection converged and one that did not
    /// has no margin to spend. That consumer does not exist at this commit, so
    /// nothing reads this field yet and nothing can produce a `false` for it
    /// to read.
    pub converged: bool,
    pub iterations: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelaxError {
    /// The budget ran out with every pair legal and the bounds still apart.
    ///
    /// Not "with a violation still standing", which is what the sibling below
    /// is for. `relax` returns [`RelaxError::Deadlocked`] the moment `project`
    /// errs, so it can only fall through to here after a projection that
    /// returned `Ok` -- and under `Axes::IN_PLANE` an `Ok` means every pair's
    /// shortfall is at or under [`SETTLED`], which is the same threshold
    /// [`worst_violation`] tests. So `worst` is the `{0, 0, 0.0}` placeholder
    /// in every case a caller with a budget of at least one can reach, and the
    /// `Display` arm below prints prose rather than a fabricated pair.
    DidNotConverge { iterations: usize, worst: Violation },
    /// No progress, and a violation still standing. A different error because
    /// the remedy differs: constraints that contradict, not a budget that ran
    /// out.
    Deadlocked { worst: Violation },
    /// The factorisation found no positive pivot.
    ///
    /// Not the unpinned-component case, which cannot arise: the anchor on the
    /// diagonal makes `A + anchor * I` strictly diagonally dominant, so it is
    /// positive definite whether or not anything is pinned. Nor a pull whose
    /// two ends are the same body: `laplacian`'s free-free arm then has
    /// `i == j`, so its four writes all land on the one diagonal cell as
    /// `+k, +k, -k, -k` and cancel, and the right-hand side cancels the same
    /// way. A self-pull is dropped silently rather than refused -- which is
    /// worth knowing, because `signal_pulls` emits one for a gate that lists
    /// its own output as an input.
    ///
    /// What is left is a negative stiffness, deep enough to overcome the
    /// anchor: a bug in how the graph was built rather than a property of the
    /// circuit. Nothing in the tree reaches it. [`build`] is the only producer
    /// of a [`Pull`] and it always writes [`SIGNAL_STIFFNESS`], which is `1.0`;
    /// only a hand-built [`BodyGraph`] could get here.
    Unsolvable { component_row: usize },
    /// The netlist and its primitive graph do not agree well enough to build
    /// bodies from -- a gate with no primitive, a declared input with no
    /// lever.
    CannotBuild { reason: String },
}

impl std::fmt::Display for RelaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `relax` raises this after a projection that returned `Ok`, so
            // there is usually no violating pair to name and `worst` is the
            // placeholder. Rendering it anyway prints "bodies 0 and 0 are
            // 0.000 too close", which reads as a measurement of a real pair.
            RelaxError::DidNotConverge { iterations, worst } if worst.shortfall == 0.0 => write!(
                f,
                "relaxation did not converge in {iterations} iterations; every pair is legal, the springs and the lattice just never agreed"
            ),
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
                "the spring system has no positive pivot at body {component_row}, which means the graph was built wrong"
            ),
            RelaxError::CannotBuild { reason } => {
                write!(f, "cannot build bodies for this netlist: {reason}")
            }
        }
    }
}

impl std::error::Error for RelaxError {}
```

`Factorisation` is named unqualified below and comes from the `pub use` above,
not from a second `use linear::Factorisation;` -- importing the same name twice
is `E0252`, and the re-export is what keeps the solver out of `dead_code` for
the two tasks before this one.

- [ ] **Step 4: Write the matrix assembly**

```rust
/// The weighted Laplacian, with pinned bodies struck out and the step's anchor
/// on the diagonal.
///
/// Struck out rather than weighted heavily: a pinned body takes no force, so it
/// is not an unknown, and its position moves to the right-hand side. What makes
/// the result positive definite is the anchor rather than the striking-out,
/// which is the whole reason an unpinned netlist can be placed at all -- see
/// [`ANCHOR_STIFFNESS`]. A [`Factorisation`] that refuses one of these is
/// therefore reporting a graph built wrong, not a circuit free to translate,
/// and [`RelaxError::Unsolvable`] says so.
fn laplacian(graph: &BodyGraph, free: &[Option<usize>], order: usize, anchor: f64) -> Vec<f64> {
    let mut matrix = vec![0.0; order * order];
    // The anchor sits on the diagonal, which makes the matrix strictly
    // diagonally dominant and so positive definite whether or not anything is
    // pinned. That matters because `PortPlacements` defaults to empty and the
    // netlist-only path takes the default: today the viewer, through
    // `compile_planned(&netlist, &PortPlacements::default())` at
    // `viewer/src/lib.rs:862`, and `plan_from_netlist`'s own tests. (`compile()`
    // is not that caller yet -- it seeds the planner from the legacy emitter and
    // never builds a `PortPlacements` at all. Task 13 is where it switches.)
    // Without an anchor a component free to translate makes the system
    // singular, and the factorisation refuses it -- correctly, and uselessly.
    for slot in 0..order {
        matrix[slot * order + slot] += anchor;
    }
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
/// every pinned neighbour's position land here. `anchor * legal` is the `c`
/// term: every free body is also pulled toward where the projection last put
/// it, which is what makes the two bounds meet. On the first step there is no
/// "last put it" yet and `legal` is the layout `relax` was handed, legal or
/// not; from the first projection on it is the projection's own output.
fn right_hand_side(
    graph: &BodyGraph,
    free: &[Option<usize>],
    order: usize,
    axis: usize,
    anchor: f64,
    legal: &[[f64; 3]],
) -> Vec<f64> {
    let mut rhs = vec![0.0; order];
    for (index, slot) in free.iter().enumerate() {
        if let Some(slot) = slot {
            rhs[*slot] += anchor * legal[index][axis];
        }
    }
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
        // Every body, pinned ones included. `PortPlacements` fixes where a
        // port sits, not which way its cell is built -- and a pinned output
        // whose route has to leave the wrong face is exactly the case a
        // facing exists to fix.
        // Recorded before the trials, because the loop leaves the *last*
        // facing tried in the body -- comparing against that would report a
        // turn on almost every body on almost every step, and the relaxation
        // would never satisfy its convergence test.
        let was = graph.bodies[body].facing;
        let mut best = (was, f64::INFINITY);
        for index in 0..4u8 {
            let facing = crate::compile::geometry::CellFacing::from_index(index)
                .expect("0..4 is horizontal");
            graph.bodies[body].facing = facing;
            let energy = incident_energy(graph, body);
            if energy < best.1 {
                best = (facing, energy);
            }
        }
        graph.bodies[body].facing = best.0;
        if best.0 != was {
            turned = true;
        }
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
/// Solve, turn, project, pull the anchor tighter. Repeat until three things
/// are true at once: the solve and the projection agree, the legal
/// configuration has stopped moving, and no body turned.
///
/// The middle one is not decoration. An earlier draft exited on the first
/// alone, and it does not measure settling: the projection moves nothing
/// whenever no pair is violating, so a slack constraint set makes that gap
/// exactly zero however far the layout still is from where the springs want
/// it. Measured on 2026-08-13 -- `and4`'s first step already had a gap of
/// 0.0000, and seven further steps took its extent from 54x39 to 45x25. The
/// loop below says which condition does what.
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

    // The upper bound: the thing the anchor pulls toward, and from the end of
    // the first step a configuration that is legal.
    //
    // Not on entry. It starts as the layout `relax` was handed, and nothing
    // checks that: `build` seeds each body from `start` or from its pin without
    // consulting `required_separations`, and `perturb` then moves bodies by up
    // to a quarter cell. The counterexample is in this file --
    // `running_out_of_iterations_is_an_error_that_says_how_many_it_had` hands
    // `relax` three coincident anchors against a requirement of at least three
    // cells. Nothing rests on it: the seed enters only as the soft
    // `anchor * legal` term of step one's right-hand side, and `project` below
    // replaces it before anything is measured against it. What leaves the loop
    // is always projected.
    let mut legal: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();
    let mut anchor = ANCHOR_STIFFNESS;

    for iteration in 1..=effort.iterations {
        // Refactorised every step, because the anchor is on the diagonal and
        // the anchor grows. One factorisation serves all three axes -- the
        // anchor is the same for each -- so this is one `O(n^3/3)` per step
        // against three `O(n^2)` solves, which at a couple of hundred bodies
        // is not the cost worth optimising.
        let factorisation = Factorisation::of(&laplacian(&bodies, &free, order, anchor), order)
            .map_err(|error| RelaxError::Unsolvable { component_row: error.row })?;

        // The lower bound: where the springs want the bodies, given how hard
        // they are currently held to the last legal configuration.
        //
        // Only the axes this stage may move on. An earlier draft solved all
        // three and restricted only the projection, which does not hold a body
        // on its storey: springs have zero rest length, so the Y solve pulls
        // every unpinned body onto its neighbours' plane and the storeys
        // `Shape::Tall` laid out collapse.
        for axis in axes.iter() {
            let mut rhs = right_hand_side(&bodies, &free, order, axis, anchor, &legal);
            factorisation.solve(&mut rhs);
            for (index, slot) in free.iter().enumerate() {
                if let Some(slot) = slot {
                    bodies.bodies[index].position[axis] = rhs[*slot];
                }
            }
        }

        let solved: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();

        let turned = choose_facings(&mut bodies);

        if let Err(worst) = project::project(&mut bodies, &required, axes) {
            return Err(RelaxError::Deadlocked { worst });
        }
        let previous = std::mem::replace(
            &mut legal,
            bodies.bodies.iter().map(|body| body.position).collect(),
        );

        // Three conditions, and each one rules out a way of stopping early
        // that the other two allow.
        //
        // `gap` is the two bounds meeting: what the springs want and what is
        // legal have stopped disagreeing. It is emphatically *not* a settling
        // test and must not be read as one. `solved` and `legal` are the same
        // configuration either side of one `project` call, and `project` returns
        // having moved nothing whenever no pair's shortfall exceeds [`SETTLED`]
        // -- so an inactive constraint set makes `gap` exactly 0.0 however far
        // the solve just carried the bodies. What it measures is whether the
        // projection was idle.
        //
        // and4 is the demonstration, measured on 2026-08-14 from
        // `plan_from_netlist`'s anchors: its step-1 `gap` is 0.0000 while that
        // same step moves a body 20.80 cells, and the seven further steps take
        // its bounding box from 54 by 39 to 45 by 25.
        //
        // `settled` is the settling test: how far this step moved the legal
        // configuration itself, `max |legal_new - legal_prev|`. It is the
        // quantity [`ContinuousPlacement::converged`] describes, and it is what
        // makes a return mean the relaxation finished rather than that the
        // constraints happened to be slack. Without it, the exit test on a graph
        // nothing crowds reduces to `!turned` alone -- a facing condition, and
        // `build` seeds every body `CellFacing::NORTH`, so it goes false as soon
        // as one sweep has re-oriented everything. That is step *two* on a chain
        // laid out north-to-south, and it stops with the two gates 30.09 cells
        // apart against the 24.78 the nine steps produce.
        // `a_slack_constraint_set_is_not_a_settled_one` is that graph, and both
        // of those numbers are measured off it.
        //
        // `!turned` is neither of those -- it is what makes `gap` a comparison
        // of like with like. `solved` is captured before `choose_facings`, so it
        // is the spring optimum for the *old* facings, while `legal` comes back
        // from a projection run against the *new* ones. A facing is an input to
        // both sides: `attach_offset` reads `body.facing` for a socket and for a
        // pin alike, so the right-hand side the solve minimised is not the one
        // that holds after a turn, and `project` separates cells the turn has
        // already moved. The subtraction is still well-defined arithmetic; what
        // is not well-defined across a turn is reading a small `gap` as "the
        // springs and the lattice agree", because the next step's solve targets
        // different offsets and lands somewhere else.
        //
        // It is kept for that reason and not for a step count: with `settled` in
        // the test it no longer changes either reference circuit's answer.
        // Measured 2026-08-14 by deleting it -- and4 still stops at step 8 with
        // a 45 by 25 box and full_adder at step 9 with 34 by 107, both
        // identical to the three-condition run.
        //
        // It cannot spin forever: a facing is an argmin over four with a
        // lowest-index tie-break, evaluated on positions that are themselves
        // converging, so once the positions settle the argmin settles with
        // them. And the anchor grows -- as far as [`ANCHOR_CEILING`], which
        // that constant's own doc argues is far past where every measured run
        // has already exited -- so the solve is pulled arbitrarily close to
        // `legal`, which is already legal, driving `gap` and `settled`
        // together to zero.
        let gap = solved
            .iter()
            .zip(&legal)
            .map(|(wanted, allowed)| {
                (0..3)
                    .map(|axis| (wanted[axis] - allowed[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        let settled = previous
            .iter()
            .zip(&legal)
            .map(|(before, after)| {
                (0..3)
                    .map(|axis| (after[axis] - before[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        if gap < CONVERGED && settled < CONVERGED && !turned {
            return Ok(ContinuousPlacement {
                graph: bodies,
                converged: true,
                iterations: iteration,
            });
        }

        anchor = (anchor * ANCHOR_GROWTH).min(ANCHOR_CEILING);
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

Expected: 8 passed.

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
- Consumes: `ContinuousPlacement`, `RelaxError`, `Violation`, `SNAP_MARGIN`, `project::{required_separations, worst_violation}`, `planner::Anchor`, `geometry::CellFacing`. `SNAP_MARGIN` because Step 3 spends it exactly once, on the way in. The tests also name `SETTLED`, to state "exactly at the requirement" as two measurements rather than as a comment.
- Produces:
  - `pub struct SnappedNode { pub node: usize, pub anchor: Anchor, pub facing: CellFacing }`
  - `pub fn snap(placement: &ContinuousPlacement) -> Result<Vec<SnappedNode>, RelaxError>`
  - `RelaxError` gains `SurvivedSnap { worst: Violation }`
  - `RelaxError::DidNotConverge`'s doc is amended. Task 8 wrote it for its one
    producer; `snap` is a second, and neither "the budget ran out" nor "`worst`
    is the placeholder" is true of it. Step 4 below.

`SnappedNode` is keyed by **candidate node index**, not by `NodeId`. The spec's
sketch said `NodeId`, and it cannot: a bare merge's junction has no node --
`expand` produces no primitive for one -- so there is no `NodeId` to name it
by. Gate index then input index is the order `emit_primitives` reads
positionally, which is the order the answer has to arrive in anyway.

**Two things the tests have to be built to, because the obvious fixture tests
neither.**

1. **The collapse needs a node that owns two bodies.** `snap` maps over
   `anchor_body`, which is per candidate node; a `snap` that mapped over
   `rounded.bodies` instead would be per body. On a plain chain those are the
   same list in the same order, so every chain fixture passes both. The
   isolated merge is the shape where they differ -- eight bodies, seven nodes,
   `anchor_body = [0, 1, 2, 4, 5, 6, 7]` -- and it is already in the tree, as
   `build.rs`'s `an_isolated_branch_welds_its_repeater_into_the_junctions_socket`.
   Under the defect it returns eight answers with every gate after the merge
   shifted by one, which nothing downstream would catch until Task 10 wired it
   in.
2. **The rounding worst case is a pair rounding *toward* each other**, not a
   pair on half-cell boundaries. `f64::round` ties away from zero, so two
   positive coordinates on half-boundaries both round up, in unison, and their
   separation does not change at all. The case that spends the margin needs one
   body to round up and the other down. How much it can spend is arithmetic:
   `required_separations` is `3 + d/4` for a routed degree `d`, rounding leaves
   every separation an integer, and the only integer in
   `[required - 1, required]` is `required - frac(required)` whenever the
   fraction is not zero -- so the loss is at most 3/4, at `d = 3`. The fixture
   builds that corner. The test's own doc carries the derivation.

- [ ] **Step 1: Write the failing tests, and declare the module**

Create `src/compile/relax/snap.rs` with the test module below, and add
`mod snap;` to `src/compile/relax/mod.rs` in the same step -- an undeclared
file is never compiled, so Step 2 would report zero tests instead of a
failure. The re-export waits for Step 5.

`PortPlacements` is imported here rather than in `snap.rs` itself: every test
names it and `snap` never does, so a file-level import would trade a
missing-name error for an unused-import one. `SETTLED` is imported for the same
reason and only the one test uses it.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::planner::PortPlacements;
    use crate::compile::primitive_graph::expand;
    use crate::compile::relax::{relax, Axes, RelaxEffort, SETTLED};
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    /// A merge one of whose branches is *isolated*: `nb` feeds both the merge
    /// and `spy`, so that branch is shared rather than bare and `expand` gives
    /// it a repeater of its own. That repeater is a body, welded into the
    /// junction's socket, and it belongs to the merge's node rather than to a
    /// node of its own -- the only shape in the tree where a node owns more
    /// than one body.
    ///
    /// The same netlist as `build.rs`'s
    /// `an_isolated_branch_welds_its_repeater_into_the_junctions_socket`,
    /// where the weld this turns on is asserted.
    fn isolated_merge() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into(), "spy".into()],
            gates: vec![
                Gate::nor("na", &["a"]),
                Gate::nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                Gate::nor("out", &["m"]),
                Gate::nor("spy", &["nb"]),
            ],
        }
    }

    /// A two-input NOR that something reads: `m` has a routed degree of three,
    /// so its requirement is `CONDUCTOR_CLEARANCE + ROUTE_PITCH * 3 / 8 +
    /// SNAP_MARGIN` = 3.75 -- the largest quarter-cell fraction a requirement
    /// can carry, which is what makes it the worst case for rounding.
    fn wide() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![Gate::nor("m", &["a", "b"]), Gate::nor("out", &["m"])],
        }
    }

    /// One answer per node `PlanCandidate` expects, in the order it expects
    /// them: gates, then primary inputs. Per *node*, which is the collapse,
    /// and the collapse is everything `snap` does beyond rounding.
    ///
    /// The fixture is the isolated merge for exactly that reason. On `chain()`
    /// and on `wide()` -- between them, what every other test in this module
    /// uses -- `bodies` and `anchor_body` are the same list in the same order, so a `snap` that
    /// mapped over `rounded.bodies` instead would pass every one of them. Here
    /// it returns eight answers for seven nodes, with the merge's welded
    /// repeater taking a node slot and every gate after it shifted by one.
    ///
    /// It is also, unplanned, the one relaxed circuit in this module that
    /// spends the rounding margin: its tightest pair -- gate `na` and the
    /// junction -- sits within 0.001 of its requirement before rounding and
    /// 0.500 inside it after, so the `expect` below is what fails first if the
    /// margin above is ever charged twice.
    #[test]
    fn snap_answers_once_per_candidate_node_in_candidate_order() {
        let netlist = isolated_merge();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..netlist.gates.len() + netlist.inputs.len())
            .map(|index| Anchor { x: index as i32 * 20, y: 1, z: index as i32 * 16 })
            .collect();

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        // Asserted rather than assumed: this fixture says nothing at all
        // unless the two counts differ, and a `chain()` here would be a test
        // that passes against the defect it names.
        assert_eq!(
            placement.graph.bodies.len(),
            8,
            "the isolated branch's welded repeater is what makes bodies outnumber nodes"
        );
        assert_eq!(placement.graph.anchor_body.len(), 7, "five gates and two inputs");

        let snapped = snap(&placement).expect("a converged placement rounds");

        assert_eq!(snapped.len(), 7, "one answer per candidate node, not per body");
        for (index, node) in snapped.iter().enumerate() {
            assert_eq!(node.node, index, "answers are not in candidate order");
            let anchor = &placement.graph.bodies[placement.graph.anchor_body[index]];
            assert_eq!(
                node.anchor,
                Anchor {
                    x: anchor.position[0].round() as i32,
                    y: anchor.position[1].round() as i32,
                    z: anchor.position[2].round() as i32,
                },
                "node {index} did not come back at its own anchor body"
            );
        }

        // And the body the collapse dropped is somewhere else, so "one per
        // node" is a choice rather than an accident of the two coinciding.
        let merge = 2;
        let junction = placement.graph.anchor_body[merge];
        assert_eq!(
            placement.graph.nodes[merge].len(),
            2,
            "the merge's node owns its junction and its branch repeater"
        );
        let repeater = *placement.graph.nodes[merge]
            .iter()
            .find(|&&body| body != junction)
            .expect("the other one");
        assert_ne!(
            placement.graph.bodies[repeater].position.map(f64::round),
            placement.graph.bodies[junction].position.map(f64::round),
            "the repeater sits in the junction's socket, a cell off it"
        );
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

    /// An unconverged placement is refused rather than rounded, and the error
    /// names the worst violation left standing. Both halves, because the spec
    /// asks for both: a refusal that reports the placeholder pair would satisfy
    /// a variant check and tell whoever reads it nothing.
    ///
    /// The margin it would spend is not there. Three nodes on one anchor
    /// overlap outright, so there is a real pair to name.
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
        // By reference, so the `else` arm can still print the error it refused
        // to match.
        let RelaxError::DidNotConverge { worst, .. } = &error else {
            panic!("refused, but not as unconverged: {error}")
        };
        assert_ne!(worst.left, worst.right, "the error has to name a pair");
        assert!(worst.shortfall > 0.0, "and say how far short it fell");
    }

    /// The worst case rounding can hand `snap`, built rather than described: a
    /// pair sitting exactly at its requirement, positioned so that rounding
    /// moves one body up and the other *down* and the two approach by 0.75 of
    /// a cell.
    ///
    /// Both halves are load-bearing.
    ///
    /// **Exactly at the requirement**, because a wider gap never spends the
    /// margin and so tests nothing. Springs pull and separation pushes, so
    /// wherever separation is what stopped the springs a converged placement
    /// sits at the requirement, and that is the tightest thing `project` can
    /// hand over. The two `worst_violation` assertions below are what make
    /// that a measurement rather than a comment: legal against the
    /// requirement, violating against a hair more.
    ///
    /// **Toward each other**, because `f64::round` ties away from zero. Two
    /// positive coordinates on half-cell boundaries therefore both round *up*,
    /// in unison, and the separation between them does not change at all --
    /// this fixture's Z is that case, both bodies at `z = 0.5` and both
    /// landing on 1, and it costs nothing. An earlier version of this test
    /// called "every body on a half-cell boundary in all three axes at once"
    /// the place "where rounding moves one furthest"; it is the place where
    /// rounding moves them the same way.
    ///
    /// **0.75 is the most that can be lost, and the arithmetic says why.**
    /// Rounding moves each body by at most half a cell, so a separation closes
    /// by at most one, and that bound alone is all [`SNAP_MARGIN`] needs to be
    /// sound. But rounding also leaves every coordinate an integer, so the
    /// separation afterwards is an integer -- while `required_separations` is
    /// `CONDUCTOR_CLEARANCE + ROUTE_PITCH * d / 8 + SNAP_MARGIN` = `3 + d/4`
    /// for a routed degree `d`. Whenever that fraction is not zero, the only
    /// integer in `[required - 1, required]` is `required - frac(required)`,
    /// so the loss is either nothing or exactly the fraction: at most 3/4, at
    /// `d = 3`. This fixture is that corner -- `m` reads both inputs and is
    /// read by `out` -- and nothing built on `required_separations` can lose
    /// more without `d` being a multiple of four, where the fraction is zero
    /// and the two reachable losses are nothing and a whole cell.
    ///
    /// The requirement is between *cells*, not centres: `m`'s socket for `b`
    /// reaches one cell east and `out`'s socket for `m` one cell west, so the
    /// centre gap that puts the closest foreign cells exactly at the
    /// requirement is two more than the requirement itself.
    #[test]
    fn a_pair_at_its_requirement_survives_rounding_toward_each_other() {
        let netlist = wide();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 4];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = required_separations(&built);
        assert_eq!(required[0], 3.75, "`m` is the degree-three body this fixture is for");
        let gap = required[0].max(required[1]) + 2.0;
        built.bodies[0].position = [0.5, 1.0, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.0, 0.5];
        // The two levers say nothing here and are parked out of reach, on
        // integers, where rounding is the identity.
        built.bodies[2].position = [0.0, 1.0, 60.0];
        built.bodies[3].position = [0.0, 1.0, 120.0];

        assert!(
            worst_violation(&built, &required).is_none(),
            "the fixture has to be legal before rounding, or it is not a placement `project` could hand over"
        );
        let a_hair: Vec<f64> = required.iter().map(|need| need + 2.0 * SETTLED).collect();
        let pinched = worst_violation(&built, &a_hair)
            .expect("and at the requirement with nothing to spare, or it tests nothing");
        assert_eq!((pinched.left, pinched.right), (0, 1), "the wrong pair is the tight one");

        let west = built.bodies[0].position[0];
        let east = built.bodies[1].position[0];
        assert!(west.round() > west, "the west body has to round up");
        assert!(east.round() < east, "and the east body down, or they do not approach at all");
        assert_eq!(
            (east - west) - (east.round() - west.round()),
            0.75,
            "the pair has to close by the whole fraction its requirement carries"
        );

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        snap(&placement).expect("what the projection guarantees has to survive rounding");
    }

    /// And a placement tighter than the projection can produce does not
    /// survive, which is what makes the test above a claim rather than a
    /// coincidence of a generous gap.
    #[test]
    fn a_placement_tighter_than_the_projection_allows_is_caught_after_rounding() {
        let netlist = wide();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 4];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = required_separations(&built);
        // Two cells tighter than the test above: the first is the margin
        // rounding is allowed to spend, and the second is a real violation.
        //
        // Not that one cell tighter would pass `snap`. The test above is built
        // to lose 0.75 to rounding, so at one cell tighter the shipped check
        // already reports 0.75 short. Two cells is what makes this a violation
        // whichever way rounding moves the pair.
        let gap = required[0].max(required[1]) + 2.0 - 2.0;
        built.bodies[0].position = [0.5, 1.0, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.0, 0.5];
        built.bodies[2].position = [0.0, 1.0, 60.0];
        built.bodies[3].position = [0.0, 1.0, 120.0];

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        let error = snap(&placement).expect_err("this one is genuinely too tight");
        assert!(matches!(error, RelaxError::SurvivedSnap { .. }), "got {error}");
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
//!
//! **Horizontally.** [`required_separations`] adds [`SNAP_MARGIN`] to the
//! horizontal requirement and to nothing else; the vertical gate is a bare
//! `dy < CONDUCTOR_CLEARANCE` in `project::unseparated`, with no margin on it.
//! Stage 1 pays nothing for that, because `Axes::IN_PLANE` never writes a
//! fractional Y: every `dy` is an integer difference of starting storeys and
//! rounding is the identity on it. Under `Axes::ALL` it becomes live, and this
//! module is one of the two places it surfaces. The mechanism is narrower than
//! it looks, and worth stating precisely so Task 11 does not chase the wrong
//! one: a pair exempted at `dy = 2.0` can only round to `dy = 1.0` by losing
//! exactly one whole cell, which needs one body at `+0.5` and the other at
//! `-0.5` -- `f64::round` ties away from zero, so straddling `Y = 0` is the
//! only way two half-integers move apart rather than in unison. A sweep over
//! `[-3, 3]` in steps of 0.025 with cell offsets `-4..=4` found 72 such pairs,
//! and every one of them straddles zero, which no starting storey in this tree
//! produces. So the exposure is real but is the same coincidence the weld note
//! below calls "code for a coincidence". Task 11 is where the vertical
//! requirement grows its own margin, and the reason to give it one is that the
//! asymmetry exists at all -- not that a circuit has hit it.

use crate::compile::geometry::CellFacing;
use crate::compile::planner::Anchor;
use crate::compile::relax::project::{required_separations, worst_violation};
use crate::compile::relax::{ContinuousPlacement, RelaxError, SNAP_MARGIN};

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

    // Every body rounds on its own, and `f64::round` ties away from zero: a
    // body at `-0.5` lands on -1 while one at `0.5` lands on 1. So a welded
    // pair whose unit offset spans zero comes out two cells apart rather than
    // one -- rounding is not offset-preserving across the origin.
    //
    // Documented rather than handled, for two reasons that both have to hold.
    // It is reachable only at an exact `-0.5`: `satisfy` writes a welded
    // body's position as its anchor's plus an integer offset, so the two
    // always share a fractional part, and only the one fraction that is a tie
    // on both sides of zero splits them. And it is inconsequential if reached,
    // because nothing downstream reads the held body's position. The check
    // below exempts welded pairs (`project::exempt`), the collapse further
    // down answers with `anchor_body` -- the junction, never the repeater --
    // and `world_partition::resolve_node_position` re-derives the repeater's
    // cell from the junction's position and facing. Handling it would be code
    // for a coincidence, guarding an answer that is thrown away.
    let mut rounded = placement.graph.clone();
    for body in &mut rounded.bodies {
        for axis in 0..3 {
            body.position[axis] = body.position[axis].round();
        }
    }

    // The margin's claim, checked rather than trusted. The invariants exist to
    // catch real errors, not to catch the legaliser's leftovers.
    //
    // Without the margin, deliberately. `required_separations` is what the
    // *projection* enforces -- clearance, reservation, and one cell of margin
    // -- and springs pull while separation pushes, so wherever separation is
    // what stopped the springs a converged placement sits exactly at it, with
    // the margin already committed. The margin is what rounding is allowed to
    // consume; what has to survive rounding is the physical requirement
    // without it.
    //
    // Asking for it on both sides refuses placements the relaxation is
    // entitled to produce. Measured on 2026-08-14 by deleting the subtraction
    // below: `full_adder`, relaxed from `plan_from_netlist`'s anchors with
    // nothing pinned under `Axes::IN_PLANE` and converged in 9 steps, is
    // refused with bodies 20 and 21 exactly 0.500 short -- one rounding's worth
    // of a margin charged twice.
    //
    // `and4` and this module's two-gate chain say nothing either way, and it
    // is not because they are loose. A converged placement never has a pair
    // *inside* its requirement -- that is what `project` returning `Ok` means,
    // `full_adder` included -- so having none is no distinction at all. What
    // separates them is how much room is left over, and what rounding then
    // does to the pair that has least. Measured the same day, by inflating
    // every requirement by `delta` and asking `worst_violation`, in steps of
    // 0.001, for the smallest `delta` at which a pair appears: before
    // rounding, `and4`'s tightest pair and `full_adder`'s both sit within
    // 0.001 of their requirement, at the equilibrium this design is built
    // around. After rounding `and4`'s tightest pair lands exactly *on* its
    // requirement -- a rounded separation is an integer and a requirement is a
    // quarter-cell multiple, so a `delta` under 0.001 means zero -- and the
    // double charge survives that by nothing whatever, while `full_adder`'s 20
    // and 21 land 0.500 inside it. The chain this module still relaxes -- the
    // one in `a_pinned_port_snaps_to_where_it_was_pinned`, pinned at
    // (37, 1, 41) -- is the genuinely slack one: 1.850 before rounding and
    // 2.501 after, over 7 steps.
    //
    // That slack is a property of the pin rather than of the chain. The same
    // ladder unpinned fails the double charge by 0.500, at bodies 0 and 2: a
    // pinned body cannot be moved by the springs, so the pair it belongs to
    // settles wherever the pin left it rather than at the requirement.
    //
    // Two of this module's five tests fail under the double charge, and both
    // are tests built to spend the margin. The hand-built pair in
    // `a_pair_at_its_requirement_survives_rounding_toward_each_other` comes
    // out 0.750 short -- the largest fraction a requirement can carry, and
    // that test's own doc derives why. The isolated merge that
    // `snap_answers_once_per_candidate_node_in_candidate_order` relaxes comes
    // out 0.500 short between gate `na` and the junction, its tightest pair
    // having sat within 0.001 of its requirement before rounding. So the
    // relaxed half of this argument is held by a real converged circuit in
    // this module, and not only by `full_adder`, which nothing in the tree
    // runs end to end yet.
    let required: Vec<f64> = required_separations(&rounded)
        .into_iter()
        .map(|separation| separation - SNAP_MARGIN)
        .collect();
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
let them be anywhere else, and
`world_partition::resolve_node_position` re-derives the repeater's cell from
the junction's position and facing downstream anyway. That is also why the
rounding comment above documents `f64::round`'s tie-away-from-zero asymmetry
rather than handling it: a welded pair whose unit offset spans zero rounds two
cells apart instead of one, and the answer it would corrupt is one nothing
reads.

- [ ] **Step 4: Add the error variant, and amend the sibling `snap` changes**

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

`DidNotConverge` also changes, because this task gives it a second producer.
Task 8's doc says the budget ran out "with every pair legal" and concludes
`worst` is the placeholder "in every case a caller with a budget of at least one
can reach". Both were true of `relax` alone and neither is true of `snap`, which
measures a placement that never converged and normally finds a real pair --
`an_unconverged_placement_is_refused_rather_than_rounded` asserts exactly that.
Replace it with:

```rust
    /// The relaxation never finished.
    ///
    /// Not "no progress, and a violation still standing", which is what the
    /// sibling below is for. This one has **two producers**, and `worst` means
    /// a different thing in each -- so a caller may not assume either.
    ///
    /// From [`relax`], `worst` is the `{0, 0, 0.0}` placeholder: the budget ran
    /// out with every pair legal and the two bounds still apart. `relax`
    /// returns [`RelaxError::Deadlocked`] the moment `project` errs, so it can
    /// only fall through to its own budget check after a projection that
    /// returned `Ok` -- and under `Axes::IN_PLANE` an `Ok` means every pair's
    /// shortfall is at or under [`SETTLED`], which is the same threshold
    /// [`worst_violation`] tests. There is no pair left to name.
    ///
    /// From [`snap`], it usually names a real one. [`snap`] refuses any
    /// placement whose `converged` is false and measures it as it was handed
    /// it -- and a placement that never converged is exactly where a violation
    /// can still be standing.
    /// `an_unconverged_placement_is_refused_rather_than_rounded` builds one and
    /// asserts the pair is named and the shortfall positive.
    ///
    /// The `Display` arm below therefore branches on `worst.shortfall` rather
    /// than on which producer raised it: prose for the placeholder, a measured
    /// pair otherwise.
    DidNotConverge { iterations: usize, worst: Violation },
```

and the comment on that `Display` arm with:

```rust
            // Which producer raised it decides whether there is a pair worth
            // printing -- `relax` reaches its budget check only after a
            // projection that returned `Ok` and so carries the placeholder,
            // while `snap` measures an unconverged placement and usually finds
            // a real pair. The shortfall is what tells the two apart here.
            // Rendering the placeholder anyway prints "bodies 0 and 0 are
            // 0.000 too close", which reads as a measurement of a real pair.
```

- [ ] **Step 5: Re-export**

`mod snap;` landed in Step 1; `src/compile/relax/mod.rs` now also carries
`pub use snap::{snap, SnappedNode};`.

- [ ] **Step 6: Run the tests**

Run: `cargo test --release --lib compile::relax::snap`

Expected: 5 passed -- the margin's two halves, `snap`'s two answers (the
collapse and the pinned port), and the refusal.

Then check each test against the defect it names, because a test that cannot
fail is not evidence:

- Map over `rounded.bodies` instead of `anchor_body`.
  `snap_answers_once_per_candidate_node_in_candidate_order` fails on "one
  answer per candidate node, not per body", and it is the only failure in the
  whole `--lib` suite.
- Delete the `- SNAP_MARGIN`. Two tests fail, both built to spend the margin:
  the hand-built pair by 0.750, and the isolated merge -- which turns out to be
  the one relaxed circuit in this module that also spends it -- by 0.500,
  between gate `na` and the junction.

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
- Modify: `src/compile/planner.rs` -- imports (4, 6), `PlannerError` (448), `plan_from_netlist_shaped` (1797-1927), `emit_primitives` (562-677), `PlanCandidate::with_primitive_nodes` (332-347)
- Modify: `src/compile/planner.rs` again, for Step 7's socket question -- `route_endpoints` (1108-1163), `node_for_gate` (1177-1181), `try_move`'s rebuild loop (776), `terminal_socket` (1559-1566)
- Test: `src/compile/planner.rs`, including the existing `moving_a_primitive_reserves_its_destination_before_rerouting_fanout` (3391-), which calls `terminal_socket` directly and has to ask the same question the rebuild loop now asks

**Interfaces:**
- Consumes: `relax::{relax, snap, Axes, RelaxEffort, SnappedNode, RelaxError}`.
- Produces:
  - `PlanCandidate::with_facings(anchors, primitive_nodes, routes, facings: Vec<CellFacing>) -> Self` -- the constructor `variant_indices` never had.
  - `PlanCandidate::node_index_for_gate(&self, gate: &str) -> Option<usize>` -- a gate's *name* to its candidate node index, which is what `facing_of` takes. `RouteSink` records the name; nothing on a route records the index.
  - `PlanCandidate::declared_socket(&self, support: Anchor, sink: &RouteSink) -> Anchor` -- the cell a declared sink's route must arrive in. Step 7's whole subject. The support is passed in rather than looked up, so that `route_endpoints`' existing remapping of a moved primitive's anchor stays in one place.
  - `PlannerError` gains `Relaxation(RelaxError)`, with a `Display` arm that
    forwards: `write!(f, "{error}")`. `RelaxError` already says which bodies
    and by how much, and wrapping that in a second sentence would bury it.
    Its derive at `planner.rs:448` drops `Eq` -- it becomes
    `#[derive(Debug, Clone, PartialEq)]` -- because `RelaxError` carries
    `Violation::shortfall: f64` and `f64: Eq` does not hold. Nothing wants it:
    `PlannerError` derives neither `Hash` nor `Ord`, so it is never a map key
    nor sorted, and every comparison in the planner's tests is `assert_eq!` or
    `matches!`, which need only `PartialEq` and `Debug`.
  - `plan_from_netlist` and `plan_from_netlist_shaped` keep their signatures.
  - `fn starting_layout(netlist: &Netlist, placements: &PortPlacements, shape: Shape) -> Result<Vec<Anchor>, PlannerError>` -- the existing depth-and-barycentre code, extracted verbatim.

- [ ] **Step 1: Write the failing tests**

**A `relax` -> `snap` test on `full_adder` belongs in this task**, and Task 9
could not write it because nothing called `snap` yet. `full_adder` is the only
reference circuit whose converged placement genuinely spends the rounding
margin: measured 2026-08-14, relaxed from `plan_from_netlist`'s anchors with
nothing pinned under `Axes::IN_PLANE`, it converges in 9 steps with its
tightest pair within 0.001 of its requirement, and after rounding bodies 20 and
21 sit 0.500 *inside* the full requirement -- so it is the one circuit that
refuses if `snap` ever charges `SNAP_MARGIN` on both sides. `and4` is not: its
tightest pair lands exactly *on* its requirement after rounding and survives
the double charge by nothing at all, which is a pass that says nothing.
Everything holding that arithmetic today is inside `snap.rs` and either
hand-built or a four-gate fixture; once this task hands `snap` a real relaxed
`full_adder`, a test that asserts `snap` returns `Ok` for it is what keeps the
margin honest against real geometry rather than against a fixture built to it.

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
    // planner.rs imports `BlockState`, not `BlockKind`; the two existing tests
    // that count non-air cells (4085, 4539) import it per function, and so
    // does this one.
    use crate::redstone::world::block::BlockKind;

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

Expected: the second fails on its two assertions, since rows and barycentres
produce exactly 572 and 24.

The first already passes, and that is the point of writing it now.
`how_far_the_planners_own_placement_carries` (planner.rs:4179) records that
and4 and full_adder place, route and verify under today's row-and-barycentre
placement -- it is `#[ignore]`d for segment_a's sake, not theirs -- and this
test is those two circuits without segment_a. It is the guard, not the driver:
it says relaxation may not lose routability the old placement had, it must
stay green through every step below, and Task 11 Step 6 is where it can
actually break. The second test is this task's RED driver.

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
///
/// It is therefore part of the design rather than scaffolding, and changing it
/// changes the answer. Measured on 2026-08-13: from this layout and4's anchors
/// occupy 1,035 cells against the 4,095 they started in; from a plain grid
/// `seven_segment` reaches 8,475 from 24,973 -- a different shape, not a worse
/// one, and neither is "the" optimum. Whoever replaces this function is
/// changing what relaxation finds, not just where it begins.
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

`relax` and `primitive_graph` are not names in planner.rs's scope: line 4
imports items *out of* `primitive_graph`, not the module, and line 6's
`{self, ..}` binds only `compile`. Add the module names first, or every
`relax::` and `primitive_graph::` below is an unresolved path:

```rust
use crate::compile::primitive_graph::{self, reexpand_gate, EntrySelection, NodeId};
use crate::compile::{self, geometry, relax, CompiledCircuit, LegacyEmission, Netlist};
```

(Task 3 Step 7 already added `geometry` to line 6; this widens it again for
`relax`.)

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
    ///
    /// One thing already reads that field: `gate_efforts` (planner.rs:2967)
    /// copies it into the `GateEffort::variant` diagnostic. So a candidate
    /// built here reports a non-zero variant for every gate relaxation turned,
    /// where before it always reported zero. Nothing scores or branches on it,
    /// and `gate_effort_reports_route_terminal_and_variant_costs_by_gate`
    /// (planner.rs:3744) keeps passing because its fixture still builds through
    /// `with_primitive_nodes`.
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

- [ ] **Step 6: Tell `compile_planned` what facings it just built**

This is the step that keeps two records of the same fact from disagreeing, and
it belongs here rather than in Stage 3 because **this** is the task where the
second record starts being wrong.

`CompiledCircuit::gate_facings` is what Task 3 gave the three verifiers,
`world_partition` and `routing_stats` so they would stop assuming north.
`compile_planned` fills it with north today (the `gate_facings:` line in its
`CompiledCircuit` literal, `mod.rs:6519` at the time of writing), which was
true until the line above -- and `compile_planned` is the one function that
reaches `plan_from_netlist`. From this step on it builds turned gates, so a verifier
handed north would inspect the wrong cells, and pass, because the cells it
inspects are empty rather than wrong.

`compile()` is not affected and is not changed here: it seeds from the legacy
emitter (`seed_from_legacy_parts`) and never calls `plan_from_netlist`, so its
gates really are all north until Task 13 switches it over. Its own
`gate_facings` line stays.

Line numbers in `mod.rs` have moved once per Stage 0 task and will move again;
find both by their `gate_facings:` field in the two `CompiledCircuit` literals
rather than by line.

```rust
        gate_facings: (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect(),
```

`candidate` is moved into `realise_and_verify`, so read the facings out before
that call and bind them:

```rust
    let candidate = planner::plan_from_netlist(netlist, placements).map_err(planner_error)?;
    let gate_facings: Vec<geometry::CellFacing> =
        (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect();
    let size = planner::candidate_world_size(&candidate);
    let realised = planner::realise_and_verify(&candidate, netlist, size).map_err(planner_error)?;
```

- [ ] **Step 7: A planned circuit reports the facings it was built at**

```rust
    /// The two records of a gate's facing -- the candidate's `variant_indices`
    /// and the compiled circuit's `gate_facings` -- have to agree, because the
    /// verifiers read the second to check what the first built.
    #[test]
    fn a_planned_circuit_reports_the_facings_it_was_built_at() {
        let netlist = crate::verilog::and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("and4 places by relaxation");
        let compiled = crate::compile::compile_planned(&netlist, &PortPlacements::default())
            .expect("and4 compiles through the planner");

        let expected: Vec<_> = (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect();
        assert_eq!(compiled.gate_facings, expected);
        assert!(
            expected.iter().any(|&facing| facing != geometry::CellFacing::NORTH),
            "relaxation turns something in and4, or this test proves nothing"
        );
    }
```

The second assertion is the one that matters. Without it the test passes
against a `compile_planned` that still hardcodes north, because
`plan_from_netlist` would have to have chosen north everywhere for the first
assertion to hold -- and if it did, this task did not do its job.

- [ ] **Step 8: Make `emit_primitives` build to the recorded facing**

At `planner.rs:613` and `621`, pass `candidate.facing_of(index)` to
`place_merge_gate` / `place_nor_gate` and to `output_pin`. At `641`, pass it to
`place_primary_input`. This is where Task 3's `facing_of` stops returning north
for everything.

- [ ] **Step 9: Settle the socket disagreement Task 3 left**

`try_move`'s `terminal_socket` (1559) picks a socket from the source-to-support
delta; `route_in_order` picks `input_directions(facing)[input_index]`. With
facings varying they disagree more often, and the router's answer is the correct
one -- it is the socket the netlist declared, and `equivalence` checks exactly
that.

The rebuild loop cannot simply be handed that lookup, because it has neither
argument. `route_endpoints` (1108-1163) returns `(Anchor, Vec<Anchor>)`: bare
supports, with the sink identity dropped on the way out. And the identity it
drops is `RouteSink { gate: String, input_index, anchor }` -- a gate *name*,
where `facing_of` takes a candidate node index. So this step threads the sink
through and adds the one lookup that is missing:

```rust
    /// A gate's name to the candidate node index that holds its facing.
    ///
    /// `node_for_gate` answers the same question with an anchor, and every
    /// caller of it wants the anchor. This one exists because a `RouteSink`
    /// records a name and `facing_of` takes an index, and there is nothing on a
    /// route that carries the index itself.
    fn node_index_for_gate(&self, gate: &str) -> Option<usize> {
        self.primitive_nodes
            .iter()
            .position(|node| node.id == format!("gate:{gate}"))
    }

    /// The cell a declared sink's route has to arrive in: `support`'s socket
    /// for the declared input this sink feeds.
    ///
    /// The netlist's answer, not the geometry's. `terminal_socket` guesses one
    /// from the direction the route approached out of, which was right while
    /// every gate faced north and every socket was in a fixed place; with
    /// facings varying it names a different cell from the one
    /// `route_in_order` laid dust to and `equivalence` checks.
    ///
    /// `support` is a parameter rather than another `node_for_gate` call
    /// because `route_endpoints` has already remapped it for the primitive it
    /// is moving, and that remapping should exist once.
    fn declared_socket(&self, support: Anchor, sink: &RouteSink) -> Anchor {
        let facing = self
            .node_index_for_gate(&sink.gate)
            .map(|node| self.facing_of(node))
            .unwrap_or_default();
        step(
            support,
            compile::geometry::input_directions(facing)[sink.input_index],
        )
    }
```

`route_endpoints` returns those sockets rather than leaving them to be derived,
so the rebuild loop has nothing left to guess. Its second element becomes
`Vec<(Anchor, Anchor)>` -- support and socket, in that order -- and its two arms
fill it differently:

- the declared arm (1152-1164) keeps its `match` exactly as it is, binds the
  result as `support`, and pairs it with
  `self.declared_socket(support, &terminal.sink)`:

```rust
                .map(|terminal| {
                    let support = match self.node_for_gate(&terminal.sink.gate) {
                        // Already moved with its node, as above.
                        Some(anchor) => anchor,
                        None if moved_primitive < self.anchors.len()
                            && terminal.sink.anchor == old_anchor =>
                        {
                            new_anchor
                        }
                        None => terminal.sink.anchor,
                    };
                    (support, self.declared_socket(support, &terminal.sink))
                })
```

- the fallback arm (1138-1150) is for a route with **no declared terminals**,
  which is a hand-built fixture rather than anything `route_every_net`
  produces -- `route_in_order` pushes a `RouteTerminal` for every consumer it
  routes to. There is no declared input index to ask for, so it keeps
  `terminal_socket(source, support)` and pairs that with its one support.

So `terminal_socket` and `preferred_axis_direction` survive, scoped to that one
arm, with a doc comment saying they are the answer of last resort for a route
whose sink the netlist never declared. Deleting them outright, as an earlier
draft of this step said, would leave that arm with nothing to call.

The rebuild loop at 769-776 then reads:

```rust
        let (source, terminals) = moved.route_endpoints(route_index, primitive, from, to);
        ...
        let mut branches = Vec::with_capacity(terminals.len());
        for (support, terminal) in terminals {
```

and drops its `let terminal = terminal_socket(source, support);` line entirely.
The rest of the body is untouched: `deterministic_astar(source, terminal,
support, ..)` still wants both, and now gets the socket the netlist declared
instead of the one the approach direction implied.

`moving_a_primitive_reserves_its_destination_before_rerouting_fanout` (3391-)
calls `terminal_socket(source, other_sink)` to predict where `try_move` will aim,
so it has to predict the new way:

```rust
        let other_terminal =
            seed.declared_socket(other_sink, &seed.routes()[0].terminals()[0].sink);
```

Its fixture already carries the sink it needs -- `RouteSink { gate: "other",
input_index: 0, .. }` -- and builds through `with_primitive_nodes`, so every
facing is north and the socket comes out at the same cell the geometric guess
did. The test's assertions do not move; what moves is which question it asks.

- [ ] **Step 10: Run the tests**

Run: `cargo test --release --lib compile::planner::tests::relaxation`

Expected: both PASS -- the second newly, the first still. A failure in the
first means relaxation lost routability that rows and barycentres had, which is
a different and worse result than missing a number.

If the second fails, record the numbers it did produce in the commit message:
the design's own condition is that failing to beat 572 and 24 means it failed
at the thing it was written for, and that is a result to report rather than a
test to weaken.

- [ ] **Step 11: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`. `compile()` still seeds from legacy, so the four pinned
sizes are unchanged. `how_far_the_planners_own_placement_carries` is still
ignored; run it by hand to see how far relaxation now carries:

```bash
cargo test --release --lib compile::planner::tests::how_far_the_planners_own_placement_carries -- --ignored --nocapture
```

- [ ] **Step 12: Commit**

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
cheaper. That asymmetry is already in `offence` from Task 7; this task is
what lets `separate` act on it.

**Two things the one word makes reachable.** Both are in `project.rs` today and
neither can fire while `Axes::IN_PLANE` is what gets passed. Task 7 shipped them
knowingly rather than guessing at a fix no test could have failed against; they
are this task's to handle, and the reasoning for each is why.

1. **`project` can return `Ok(())` over a live violation.** The pair loop skips a
   pair whose chosen amount is `<= SETTLED`, and that amount is the *chosen
   axis's* deficit, not the pair's shortfall. Under `IN_PLANE` those cannot
   disagree: every axis in that set is charged the same target, so for the cell
   pair `p` that sets the shortfall, `required - |Δx_p|` and `required - |Δz_p|`
   are both at least `required - max(|Δx_p|, |Δz_p|)`, which is the shortfall
   itself -- so every axis's deficit, and therefore the cheapest, is at least the
   Chebyshev shortfall, and a pair that got past the shortfall test cannot fail
   the amount test. That proof breaks on exactly one word: axis 1 is charged
   [`CONDUCTOR_CLEARANCE`] rather than the pair's requirement. A pair at
   `dy = 1.9999999` has a Y deficit of `1e-7`, below [`SETTLED`], while its
   horizontal shortfall may be metres. `cheapest_axis` picks Y, the guard skips
   the pair, nothing else moves it, `moved` stays false, and `project` returns
   `Ok(())` with the violation still standing. The invariant to restore: if
   `worst_violation` would name a pair, `project` may not return `Ok`.
2. **The vertical requirement has no rounding margin.** `required_separations`
   adds [`SNAP_MARGIN`] to the horizontal requirement because rounding moves each
   body by up to half a cell, so a gap can close by one and a continuous
   separation of `r` can land at `r - 1`. The vertical target -- `unseparated`'s
   `dy < CONDUCTOR_CLEARANCE` and `offence`'s charge on `deficit[1]`, which must
   stay the same number -- pays nothing for that. In Stage 1 it costs nothing to
   omit: no body's Y ever *changes*, so every `dy` is an integer difference of
   starting storeys and rounding is the identity on it. Written, yes -- `satisfy`
   assigns a welded body's whole `position` -- but every weld `build` produces
   has a horizontal offset, so the value it writes back is the one already
   there. The moment `separate` can
   write a fractional Y, a pair sitting at the [`SETTLED`]-legal edge of `2.0`
   with both ends near a half-cell boundary rounds to `1` -- and `1` is the gap
   [`CONDUCTOR_CLEARANCE`] exists to forbid. The derivation of the vertical
   requirement is not in question, only the margin: `CONDUCTOR_CLEARANCE` applied
   to an axis is still the right number, and the rounding argument that buys the
   horizontal one a margin applies to it verbatim.

   **It surfaces twice, and the second place is `snap`.** The paragraph above is
   the direct harm -- a stack that rounds into the gap the clearance forbids.
   The other is a refusal of something legal. `unseparated` gates the
   *horizontal* requirement behind `dy < CONDUCTOR_CLEARANCE`, so a pair sitting
   `dy = 2.0` apart is exempt from it entirely however close it is in plan. Let
   that pair round to `dy = 1.0` and the horizontal requirement switches on
   underneath it -- reservations and all, which is far more than the two cells
   the vertical gate was asking for -- and `snap`'s post-rounding check reports
   `SurvivedSnap` on a placement the relaxation was entitled to produce.
   `snap.rs`'s module doc records the same thing from its own end. The margin on
   the vertical target fixes both: with it, a pair legal at `dy = 3.0` cannot
   round below `2.0`, so the horizontal gate never opens on a pair that was
   exempt from it. Worth a third test here, on the `snap` side rather than the
   `project` side.

Adding the margin moves `two_bodies_in_one_column_are_left_where_they_are`'s two
bodies: a pair exactly [`CONDUCTOR_CLEARANCE`] apart in Y stops being separated
and starts being pushed. That test's fixture has to state the vertical
requirement this task ships, not the one Task 7 shipped.

**Files:**
- Modify: `src/compile/planner.rs` -- `Shape` (1731-1751) deleted, `plan_from_netlist_shaped` merged into `plan_from_netlist`, `TALL_COLUMN_LIMIT` (1754) deleted, the test at 4299-4329 replaced
- Modify: `src/compile/relax/mod.rs` -- add the `project_for_test` module Step 1 needs; `Axes::ALL` already exists
- Modify: `src/compile/relax/project.rs` -- the two above, and a test for each
- Test: `src/compile/planner.rs`, `src/compile/relax/project.rs`

**Interfaces:**
- Consumes: `relax::Axes::ALL`.
- Produces: `plan_from_netlist(netlist, placements)` is the only entry point.
  `Shape`, `plan_from_netlist_shaped` and `TALL_COLUMN_LIMIT` are **deleted**.
  `starting_layout` loses its `shape` parameter and always lays one storey.
- Produces: `#[cfg(test)] pub(crate) mod project_for_test` in `relax/mod.rs`,
  holding one item --
  `pub fn two_free_bodies(a: [f64; 3], b: [f64; 3]) -> BodyGraph` -- two
  unwelded, unpinned `BodyKind::Primitive` bodies at `a` and `b`, each on its
  own net, assembled exactly as project.rs's own
  `graph_of(vec![body(..), body(..)], Vec::new())` from Task 7 Step 1. Only the
  fixture: `project` and `required_separations` are already re-exported from
  `relax` (Task 7 Step 7), so the test calls them directly. `#[cfg(test)]`
  because a fixture is not something the shipping crate should offer.
- Produces: no signature change in `project.rs`. Two behaviour changes, both
  invisible under `Axes::IN_PLANE` and both stated above: the vertical
  requirement becomes `CONDUCTOR_CLEARANCE + SNAP_MARGIN` wherever it is
  written, and `project` stops returning `Ok` over a pair `worst_violation`
  would name.

- [ ] **Step 1: Write the fixture, and the failing tests**

The fixture goes in first and in this same step. `planner`'s test names
`project_for_test::two_free_bodies`, and a module no step writes is an `E0432`
on the `use` line -- which fails the whole test target, so Step 2 would report a
test that did not compile rather than the failure it is looking for. Every other
task in this plan declares its module in the step that first names it; this is
that step.

Add to `src/compile/relax/mod.rs`, after the re-exports:

```rust
/// One fixture, for a test that lives in `planner`.
///
/// `project.rs` has this graph already, as its own `graph_of(vec![body(..),
/// body(..)], Vec::new())`, but a test module is not reachable from another
/// crate module -- and what `planner`'s test needs is to state a body graph in
/// two coordinates rather than to build six `Body` fields by hand. `#[cfg(test)]`
/// because a fixture is not something the shipping crate should offer.
#[cfg(test)]
pub(crate) mod project_for_test {
    use super::{Body, BodyGraph, BodyKind};
    use crate::compile::geometry::CellFacing;
    use crate::compile::topology::Primitive;

    /// Two unwelded, unpinned bodies, each on its own net.
    ///
    /// Its own net, because two cells carrying the same signal are exempt from
    /// separation -- the route between them is what makes them one thing -- and
    /// a fixture that shared a name would be testing the exemption instead.
    pub fn two_free_bodies(a: [f64; 3], b: [f64; 3]) -> BodyGraph {
        let body = |position: [f64; 3]| Body {
            what: BodyKind::Primitive { node: 0, kind: Primitive::Torch },
            position,
            inputs: vec![format!("net{}{}{}", position[0], position[1], position[2])],
            output: None,
            facing: CellFacing::NORTH,
            pinned: false,
        };
        BodyGraph {
            bodies: vec![body(a), body(b)],
            pulls: Vec::new(),
            welds: Vec::new(),
            nodes: vec![vec![0], vec![1]],
            anchor_body: vec![0, 1],
        }
    }
}
```

Then replace `a_tall_preference_uses_height_where_a_wide_one_uses_floor` in
`planner.rs`'s test module with:

```rust
/// A pair a storey apart in Y is already separated, and the projection moves
/// neither.
///
/// Two cells, not the safety condition's one. That condition has no
/// pure-vertical case -- every case of `dust_reach` takes a horizontal cardinal
/// step -- but `dust_reach` is the join mechanism, and power arriving from the
/// dust above a block is a different one nobody has derived here. So the
/// vertical requirement is `CONDUCTOR_CLEARANCE` applied to an axis, which is
/// what `offence` enforces and what the spec's test 8 asks for.
///
/// Plus `SNAP_MARGIN`, for the reason every horizontal requirement carries it:
/// rounding moves each body by up to half a cell, so a gap can close by one.
/// Stage 1 never wrote a fractional Y and so never had to pay for that; this
/// task does.
///
/// It is still far cheaper than the horizontal requirement, which carries the
/// routing reservation on top -- and that gap is the mechanism the next test
/// buys with, tested where it can be seen rather than inferred from a layout.
#[test]
fn two_bodies_in_one_column_are_left_where_they_are() {
    use crate::compile::relax::{
        project, project_for_test, Axes, CONDUCTOR_CLEARANCE, SNAP_MARGIN,
    };

    let storey = CONDUCTOR_CLEARANCE + SNAP_MARGIN;
    let mut graph =
        project_for_test::two_free_bodies([10.0, 1.0, 10.0], [10.0, 1.0 + storey, 10.0]);
    let required = vec![9.0, 9.0];
    project(&mut graph, &required, Axes::ALL).expect("already separated");

    assert_eq!(graph.bodies[0].position, [10.0, 1.0, 10.0]);
    assert_eq!(graph.bodies[1].position, [10.0, 1.0 + storey, 10.0]);
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

`project` itself needs no such wrapper -- Task 7 Step 7 re-exports it from
`relax`, because a `pub` item in a private module that only tests reach is
`dead_code`. The fixture is the only thing this module adds.

Then add to `project.rs`'s own test module, one test per landmine:

```rust
/// A pair with metres of horizontal shortfall is not left where it is because
/// the vertical axis happens to be a hair from clear.
///
/// The cheapest axis is Y by a wide margin -- a deficit of `1e-7` against
/// eleven cells on either horizontal axis -- and `1e-7` is below `SETTLED`. A
/// projection that reads that as "settled" skips the pair, moves nothing else,
/// and reports the round as quiet.
#[test]
fn a_pair_nearly_clear_in_y_is_still_separated() {
    let mut graph = graph_of(
        vec![
            body(0.0, 1.0, 0.0),
            body(0.0, 1.0 + CONDUCTOR_CLEARANCE - 1e-7, 0.0),
        ],
        Vec::new(),
    );
    let required = vec![9.0, 9.0];
    project(&mut graph, &required, Axes::ALL).expect("two bodies always fit");
    assert!(
        worst_violation(&graph, &required).is_none(),
        "reported success over {:?}",
        worst_violation(&graph, &required)
    );
}

/// The vertical requirement buys the same rounding margin the horizontal one
/// does, in both places it is written.
///
/// Two numbers, and a projection is only safe if they are the same one:
/// `unseparated` decides whether a pair offends, and `offence` decides what
/// clearing it along Y costs. A margin added to one and not the other is a
/// projection that moves a pair to a gap it still calls a violation.
#[test]
fn the_vertical_requirement_carries_a_rounding_margin() {
    let below = body(0.0, 1.0, 0.0);
    let above = body(0.0, 1.0 + CONDUCTOR_CLEARANCE, 0.0);
    let mut graph = graph_of(vec![below, above], Vec::new());
    let required = vec![9.0, 9.0];
    project(&mut graph, &required, Axes::ALL).expect("two bodies always fit");

    let dy = (graph.bodies[0].position[1] - graph.bodies[1].position[1]).abs();
    assert!(
        dy >= CONDUCTOR_CLEARANCE + SNAP_MARGIN - SETTLED,
        "a stack rounding half a cell each way closes by one, and this is {dy}"
    );
}
```

`SNAP_MARGIN` and `SETTLED` are both already on `relax/mod.rs`'s re-export list
from Task 7 Step 7, and `project.rs`'s test module reaches them through
`use super::*` either way.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --release --lib compile::planner::tests::crowding_produces_height`

Expected: FAIL on its assertion -- relaxation is still in-plane, so six gates
spread sideways and `height` is 1. A compile error here means the fixture from
Step 1 is missing, which is a different failure and not the one this step is
looking for.

Run: `cargo test --release --lib compile::relax::project`

Expected: the two new tests FAIL and the seven from Task 7 pass.
`a_pair_nearly_clear_in_y_is_still_separated` fails on its assertion rather than
on `expect` -- the bug being caught is precisely that `project` calls this a
success.

- [ ] **Step 3: Let separation reach for the third dimension**

In `plan_from_netlist_shaped`, `relax::Axes::IN_PLANE` becomes
`relax::Axes::ALL`. Not `plan_from_netlist`: Task 10 Step 4 put the `relax` call
in the shaped entry point, and `plan_from_netlist` is a two-line delegation to
it until Step 4 below folds the two together.

Then the two things that word makes reachable, both in `project.rs`:

- **The amount guard.** `project` may not skip a pair `worst_violation` would
  name. Every axis deficit is strictly positive for an offending pair --
  `unseparated` requires `dy < CONDUCTOR_CLEARANCE` and `max(|Δx|, |Δz|) <
  required`, so each axis is strictly short of its own target -- so there is
  always a move available, and the question is which guard is the right one, not
  whether one is needed. Whatever it becomes, `a_pair_nearly_clear_in_y_is_still_separated`
  is what says it worked.
- **The vertical margin.** [`SNAP_MARGIN`] on the vertical target, in
  `unseparated`'s `dy <` test and in `offence`'s charge on `deficit[1]`, which
  have to remain the same number for the reason
  `the_vertical_requirement_carries_a_rounding_margin` states. Step 1's
  `two_bodies_in_one_column_are_left_where_they_are` already states the vertical
  requirement this task ships rather than Task 7's, which is why it is the one
  new test that does not fail first.

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

Run: `cargo test --release --lib compile::relax::project`

Expected: 9 passed -- Task 7's seven and Step 1's two.

- [ ] **Step 6: Re-run the corridor test at height**

Run: `cargo test --release --lib compile::planner::tests::relaxation_routes_everything_the_old_placement_could`

Expected: PASS. This is the one that can regress here: routes need floors, and
a body that moved up may now sit where a floor has to go. If it fails, the
routing reservation is what to look at first -- the vertical requirement
carries none, which is deliberate and is exactly the assumption under test.

- [ ] **Step 7: Run the whole suite**

Run: `./check.sh`

Expected: `failed=0`, three more tests than before -- the shape test went out,
two came into `planner` and two into `project`, and Step 4's deletions remove no
other test.

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
- Create: `viewer/tests/fixtures/and4_placement.txt` -- the native answer, committed. `include_str!` expands at compile time, so this file is a build input rather than an output, and Step 3 creates it empty before anything can run.
- Modify: `viewer/Cargo.toml` -- `wasm-bindgen-test` under the `[dev-dependencies]` heading that exists at 16-17 with nothing under it
- Modify: `src/compile/planner.rs` -- a `pub fn placement_fingerprint(candidate: &PlanCandidate) -> String`
- Test: all of the above

**Interfaces:**
- Produces: `pub fn placement_fingerprint(candidate: &PlanCandidate) -> String` -- every anchor and facing in candidate-node order, as text, so two toolchains can be compared by string rather than by float.
- Consumes: `wasm-bindgen-test = "0.3"`, a **dev**-dependency of `viewer/` only.
  The Global Constraint is that the *solver* takes no dependency -- it is about
  what ships in the wasm bundle and what could make two toolchains disagree.
  This is the harness `wasm-pack test` will not run without, it is never built
  into `viewer/pkg/`, and its version series is the one that pairs with the
  `wasm-bindgen = "0.2"` already in `[dependencies]`.

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

- [ ] **Step 2: Write the failing test, and give wasm a harness that can run it**

Add to `viewer/Cargo.toml`, under the empty `[dev-dependencies]` at 16-17:

```toml
wasm-bindgen-test = "0.3"
```

Without it `wasm-pack test` refuses to start at all -- it checks for the
dependency by name and says so. And with it but nothing else, the run would be
worse than a failure: on `wasm32-unknown-unknown` the libtest harness collects
no `#[test]` functions, so a plain `#[test]` reports zero tests and exits 0.
That is the "a test that did not run rather than a test that failed" this plan
refuses three separate times, and it is the one failure mode a task whose whole
purpose is to make a disagreement loud cannot have.

So the test carries both attributes, one per target. `check.sh` runs
`cd viewer && cargo test --release` natively and
`cargo clippy --all-targets -- -D warnings` after it, so the import has to be
gated too: an unconditional `use wasm_bindgen_test::wasm_bindgen_test;` is an
unused import on the native build, and `-D warnings` makes that an error.

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

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

/// One function, two harnesses. `wasm-bindgen-test-runner` collects only
/// `#[wasm_bindgen_test]`, and `cargo test` on the host collects only
/// `#[test]` -- so a single attribute means one of the two runs nothing and
/// says nothing, which is the outcome this whole task exists to rule out.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
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

Create `viewer/tests/fixtures/and4_placement.txt` **empty, first**.
`include_str!` is expanded at compile time, so with the file missing the test
crate does not build -- rustc reports "couldn't read ...", no binary is
produced, and there is no computed fingerprint to capture.

Then run:

```bash
cd viewer && cargo test --release --test placement_agrees_with_native
```

The assertion fails and prints the fingerprint it computed as the `left` side
of the `assert_eq!`. Write that text into the fixture and re-run: PASS. The
fixture is the native answer; the test's job is to make any other toolchain
disagree loudly.

`include_str!` stays rather than a runtime `read_to_string`: Step 4 runs the
same test under `wasm-pack test --node`, where a relative file read would not
resolve to the same path.

- [ ] **Step 4: Run it under wasm**

Run:

```bash
cd viewer && wasm-pack test --node -- --test placement_agrees_with_native
```

Expected: `1 passed`. **Read the count, not just the exit status.** A run that
reports zero tests is this task failing silently -- either the dependency or the
`#[wasm_bindgen_test]` attribute from Step 2 did not land -- and it exits 0
either way.

**If the assertion fails**, do not adjust the fixture. Record the
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
- Test: `src/compile/mod.rs` (in-file `#[cfg(test)] mod tests`), `tests/reference_circuits.rs`, `tests/compile_end_to_end.rs`

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

In `src/compile/mod.rs`'s own test module, **not** in `tests/`. The property
the switchover is about is only visible through `legacy_emission()`, which is
`pub(crate)` (mod.rs:1420) and returns the `pub(crate)` `LegacyEmission`
(mod.rs:1432); `tests/reference_circuits.rs` is a separate crate and cannot see
either. Of the two ways out -- widen the API or move the test -- move the test:
`legacy_emission()` cannot be made `pub` without also exporting
`LegacyEmission` (that is `E0446`, a private type in a public interface), and
`planner_kind()` is no substitute because it already returns
`PlannerKind::Unified3d` today, which is exactly what
`every_reference_circuit_ships_the_planners_realisation`
(tests/reference_circuits.rs:286) asserts. The one thing that changes at the
switchover is `legacy_emission` going from `Some` to `None`, so the test has to
live where that name resolves.

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
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};

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

Run: `cargo test --release --lib compile::tests::no_reference_circuit_goes_through_the_legacy_emitter`

Expected: FAIL on the first circuit -- `compile` still seeds from legacy, so
`legacy_emission()` is `Some`.

- [ ] **Step 4: Rename the legacy path rather than deleting it**

The spec keeps legacy **as a comparison, not as the production path**. Deleting
it would leave Step 7 with nothing to compare against, so `compile`'s current
body -- `build_floorplan`, `build_nets`, `reserve_columns`,
`resolve_bypass_and_geometry`, and the seed handoff at 6324-6334 -- moves
verbatim into:

```rust
/// Compile by the row/channel/track emitter.
///
/// No longer the production path: `compile` places by relaxation. Kept because
/// "is relaxation better" is a question somebody will ask again, and a
/// comparison nobody can run is a claim nobody can check.
///
/// This stamps `PlannerKind::Legacy`, which nothing constructed before today.
pub(crate) fn compile_legacy(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    let order = checked_topological_order(netlist)?;
```

with its final `PlannerKind::Unified3d` becoming `PlannerKind::Legacy` and its
`gate_facings` filled with north, which is what it builds.

The up-front checks at 6138-6162 are the one part that does **not** move, because
Step 5 needs them too and the same lines cannot move to two places. They are
lifted out sideways instead, into something both paths call:

```rust
/// Every check `compile` used to make before it started building, and the
/// topological order proving the last of them.
///
/// Shared rather than moved, because both compilers want it: the emitter still
/// has to refuse an unrealisable gate, and `compile_planned` never had these at
/// all -- it relied on `plan_from_netlist` failing later, with a worse message.
///
/// The order comes back rather than being recomputed. Proving there is one *is*
/// the acyclicity check, and `build_floorplan` needs the order itself.
fn checked_topological_order(netlist: &Netlist) -> Result<Vec<usize>, CompileError> {
```

holding, unchanged, the realisability loop, the two driven-signal loops, and
`netlist.topological_order().ok_or(CompileError::CyclicNetlist)`.

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

`compile_planned` already reports its facings -- Task 10 Step 6 wired it, at
the task where they first stop being north, so there is nothing to add to its
`CompiledCircuit`.

What it does gain is the front of `compile`: the checks Step 4 lifted out,
which `compile_planned` never had any of.

```rust
    // The order itself is the emitter's; what this path wants is the three
    // refusals that come with computing it, ahead of a planner failure that
    // would describe the same netlist far less clearly.
    let _ = checked_topological_order(netlist)?;
```

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

This one goes in `src/compile/mod.rs`'s test module too, and for the same
reason as Step 2: it calls `compile_legacy`, which is `pub(crate)`. Making it
`pub` would put the retired emitter back in the crate's public API, which is
the opposite of what Step 4 says it is for.

```rust
/// What the switchover cost or saved, printed rather than asserted.
///
/// The choice recorded in the spec is to switch when placement is correct, not
/// when it is better -- so a regression here is not a failure. It is a number
/// somebody has to be able to see.
#[test]
#[ignore = "measurement, not a gate"]
fn relaxation_against_the_emitter() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};

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

Run it by hand to see the gap:

```bash
cargo test --release --lib compile::tests::relaxation_against_the_emitter -- --ignored --nocapture
```

`the_hand_written_circuits_keep_their_measured_size` counts non-air cells with
this same loop written inline and keeps its own copy: it lives in
`tests/reference_circuits.rs`, a separate crate, where `occupied` is not
nameable. Sharing it would mean exporting a test helper from the library,
which is a worse trade than one duplicated triple loop.

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
