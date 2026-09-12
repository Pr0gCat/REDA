# IO-bounded 3D Floorplan Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a complete pinned IO set define a hard X/Z footprint, keep a
clear terminal tunnel at every pin, and deterministically fold primitive and
opaque module macros into real Y-separated decks that route and certify.

**Architecture:** Complete pins close the existing lateral/forward placement
limits; incomplete pins keep the exact legacy path. The placer assigns ordered
columns to decks and reserves one row-grid vertical-trunk corridor per
cross-deck net. Seed construction materializes every deck at a derived Y,
plans channels independently per deck, unions their typed reservations, then
uses the existing router and whole-candidate certification unchanged.

**Tech Stack:** Rust 2021, existing `BTreeMap`/`BTreeSet`, existing physical
router and typed reservations, existing simulator/verifier/certifier; no new
crate or solver.

**Spec:** `docs/superpowers/specs/2026-09-12-io-bounded-3d-floorplan.md`

## Global Constraints

- Strict TDD: every behavior change starts with a focused failing test whose
  expected value is hand-derived; observe RED before production edits.
- Serialize Cargo. Never run two Cargo commands together, and never run Cargo
  while `fragment_acceptance` or a fixture generator is still alive.
- Complete, non-degenerate pin sets activate bounded mode. Partial and
  unpinned inputs keep their candidate/world/plan fingerprints and terminal
  behavior byte-for-byte.
- The global placement revision stays unchanged. Only complete bounded cases
  add the `io-bounded-3d-v1` policy marker to the case descriptor.
- The eleven checked seven-segment `(Anchor, toward)` pairs in
  `tests/build_circuit_pins.rs` never change.
- Caller cells remain air. Handover direction and logical high (`strength >
  0`) remain unchanged.
- Keep `LayoutRepair::WidenChannel` serialized shape unchanged. Bounded mode
  uses a separate `(deck, level)` repair.
- Primitive collision search stays horizontal within its assigned deck.
- Opaque blocks stay East-facing, compile once, stamp many, flatten into one
  final candidate, and pass the unchanged verifier/equivalence/simulator/
  certifier boundary.
- Do not raise router limits, add randomness, use wall-clock decisions, add a
  second router, or add post-route compaction in this plan.
- Release-only commands run only at the tasks that explicitly name them.
- After each task: run its focused GREEN command, run the legacy fingerprint
  guard, request correctness review, then commit only that task.

---

## File structure

| File | Responsibility in this plan |
|---|---|
| `src/compile/planner.rs` | Complete-pin footprint arithmetic, terminal tunnel geometry, typed pin refusals. |
| `src/compile/fragment_synth/channel_plan.rs` | Shared turnaround arithmetic and vertical-trunk lane pitch. |
| `src/compile/fragment_synth/placement.rs` | Closed bounded limits, 3D macro envelopes, ordered deck packing, deck grounds, plan-only metrics. |
| `src/compile/fragment_synth/seed.rs` | Materialize deck Y, clip all post-plan moves, reserve tunnels/trunks/perimeter, route per-deck layouts. |
| `src/compile/fragment_synth/channel_layout.rs` | One deck-local channel layout and deterministic union keys. |
| `src/compile/fragment_synth/candidate.rs` | Final exhaustive IO-footprint invariant. |
| `src/compile/fragment_synth/api.rs` | Complete-pin-only bounded policy marker. |
| `src/compile/fragment_synth/blocks.rs` | Existing `BlockBounds` source of block height; no new block format. |
| `src/compile/fragment_synth/relocate.rs` | Existing exhaustive anchor walker and existing 3D translation; no new walker. |
| `tests/build_circuit_pins.rs` | Frozen pin literals, complete/partial behavior, bounded seven-segment correctness. |
| `tests/hierarchical_synthesis.rs` | Compile-once/stamp-many and bounded hierarchical deck fixture. |
| `tests/fixtures/fragment_synth_{baseline,shipping}.json` | Regenerated acceptance evidence after the bounded trial. |
| `.superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md` | RED/GREEN commands, metrics, decisions, and final evidence. |

---

### Task 1: Freeze legacy behavior and unify turnaround arithmetic

**Files:**
- Modify: `src/compile/fragment_synth/channel_plan.rs:21-105`
- Modify: `src/compile/fragment_synth/channel_layout.rs:105-112,325-340`
- Modify: `src/compile/fragment_synth/placement.rs:193-198,629-673`
- Create: `.superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md`

**Interfaces:**
- Produces:
  ```rust
  pub(crate) const FORWARD_MARGIN: i32 = 8;
  pub(crate) const LEGACY_TURNAROUND_CHANNEL: i32 = 40;
  pub(crate) const fn legacy_turnaround_allowance() -> i32;
  pub(crate) fn bounded_turnaround_channel(lanes: usize) -> i32;
  pub(crate) fn bounded_turnaround_allowance(lanes: usize) -> i32;
  ```
- Legacy callers continue to obtain `48` and `40`; bounded callers added later
  use the lane-count functions.

- [ ] **Step 1: Record the authoritative legacy baseline**

Create the progress ledger with HEAD, the pinned refusal copied from the
checked fixture, and the existing no-block placer fingerprint:

```markdown
# IO-bounded 3D floorplan progress

## Baseline

- Base commit: `d3c17d4`
- Pinned seven-segment: uncertified; `RouteId(0)` exceeded `QueueEntries`
  while routing sink ordinal 3.
- Unpinned two-stage placement fingerprint:
  `24ef3d8e581982f52eeb7a40a6763ae2e8ad2493553aa4851fe309e55000bc93`.
- `tests/fixtures/fragment_synth_baseline.json` has `physical: null` for
  `pinned:verilog:seven_segment`; final physical fill has no old comparison.
```

- [ ] **Step 2: Add the failing shared-arithmetic test**

In `channel_plan.rs` tests:

```rust
#[test]
fn turnaround_allowance_is_channel_plus_closed_margin() {
    assert_eq!(legacy_turnaround_allowance(), 48);
    assert_eq!(
        legacy_turnaround_allowance(),
        LEGACY_TURNAROUND_CHANNEL + FORWARD_MARGIN
    );
    assert_eq!(
        bounded_turnaround_allowance(3),
        bounded_turnaround_channel(3) + FORWARD_MARGIN
    );
}
```

- [ ] **Step 3: Verify RED**

Run:

```powershell
cargo test --lib turnaround_allowance_is_channel_plus_closed_margin
```

Expected: compile failure because the four shared items do not exist.

- [ ] **Step 4: Implement the shared arithmetic without changing legacy geometry**

In `channel_plan.rs`:

```rust
pub(crate) const FORWARD_MARGIN: i32 = 8;
pub(crate) const LEGACY_TURNAROUND_CHANNEL: i32 = 40;

pub(crate) const fn legacy_turnaround_allowance() -> i32 {
    LEGACY_TURNAROUND_CHANNEL + FORWARD_MARGIN
}

pub(crate) fn bounded_turnaround_channel(lanes: usize) -> i32 {
    channel_width(lanes)
}

pub(crate) fn bounded_turnaround_allowance(lanes: usize) -> i32 {
    bounded_turnaround_channel(lanes) + FORWARD_MARGIN
}
```

Replace only the two legacy literals with the shared legacy items. Do not call
the bounded functions yet.

- [ ] **Step 5: Verify GREEN and unchanged legacy fingerprint**

Run serially:

```powershell
cargo test --lib turnaround_allowance_is_channel_plus_closed_margin
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

Expected: both pass; the literal placer fingerprint remains unchanged.

- [ ] **Step 6: Commit**

```powershell
git add src/compile/fragment_synth/channel_plan.rs src/compile/fragment_synth/channel_layout.rs src/compile/fragment_synth/placement.rs .superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md
git commit -m "refactor(synthesis): share turnaround arithmetic"
```

---

### Task 2: Complete-pin footprint and exact terminal tunnels

**Files:**
- Modify: `src/compile/planner.rs:489-546,2823-2895,3661-3779`
- Test: `src/compile/planner.rs` test module near `12273-12380`

**Interfaces:**
- Produces:
  ```rust
  /// Inclusive X/Z board bounds derived only from a complete pin set.
  /// Distinct from the test-only keep-out measurement named `Footprint` below.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) struct IoFootprint {
      pub min_x: i32,
      pub max_x: i32,
      pub min_z: i32,
      pub max_z: i32,
  }

  impl IoFootprint {
      pub(crate) fn from_complete(
          expected_ports: usize,
          caller_cells: impl IntoIterator<Item = Anchor>,
      ) -> Option<Self>;
      pub(crate) fn contains_xz(self, at: Anchor) -> bool;
      pub(crate) fn projected(self, forward: Facing, lateral: Facing)
          -> (i32, i32, i32, i32);
  }

  pub(crate) fn terminal_tunnel(pin: PortPin, role: PortRole)
      -> BTreeSet<Anchor>;
  ```
- `PinRefusal` gains `OutsideIoFootprint { cell: Anchor }` and
  `ClearanceConflict { other_port_cell: Anchor }`.

- [ ] **Step 1: Add failing pure tests**

Add tests with hand-derived coordinates:

```rust
#[test]
fn complete_partial_degenerate_and_turned_footprints_are_literal() {
    let complete = IoFootprint::from_complete(
        2,
        [Anchor { x: 10, y: 1, z: 20 }, Anchor { x: 30, y: 8, z: 50 }],
    )
    .unwrap();
    assert_eq!(complete, IoFootprint { min_x: 10, max_x: 30, min_z: 20, max_z: 50 });
    assert!(IoFootprint::from_complete(3, [Anchor { x: 10, y: 1, z: 20 }]).is_none());
    let degenerate = IoFootprint::from_complete(
        2,
        [Anchor { x: 10, y: 1, z: 20 }, Anchor { x: 10, y: 1, z: 50 }],
    )
    .unwrap();
    assert_eq!(degenerate.min_x, degenerate.max_x);
    assert_eq!(complete.projected(Facing::North, Facing::East), (-50, -20, 10, 30));
}

#[test]
fn input_and_output_tunnels_are_three_by_three_by_two() {
    let input = PortPin { at: Anchor { x: 10, y: 2, z: 10 }, toward: Facing::East };
    let output = PortPin { at: Anchor { x: 20, y: 2, z: 10 }, toward: Facing::East };
    let input_cells = terminal_tunnel(input, PortRole::Input);
    let output_cells = terminal_tunnel(output, PortRole::Output);
    assert_eq!(input_cells.len(), 18);
    assert!(input_cells.contains(&Anchor { x: 11, y: 3, z: 11 }));
    assert!(!input_cells.contains(&Anchor { x: 12, y: 2, z: 10 }));
    assert!(output_cells.contains(&Anchor { x: 19, y: 1, z: 9 }));
    assert!(!output_cells.contains(&Anchor { x: 18, y: 2, z: 10 }));
}
```

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib complete_partial_degenerate_and_turned_footprints_are_literal
```

Expected: compile failure naming `IoFootprint`.

- [ ] **Step 3: Implement footprint and tunnel arithmetic**

`from_complete` returns `None` when `expected_ports == 0` or the collected
caller-cell count differs. Otherwise it takes literal min/max X/Z. `projected`
projects all four corners with a private four-facing match identical to the
existing `placement::project_horizontal`; do not move the existing helper or
introduce a new geometry module. `terminal_tunnel` enumerates `a=0..1`, `b=-1..1`,
`c=-1..1`; it does not remove exemptions—the caller decides which claims are
required hardware.

- [ ] **Step 4: Add failing validation tests**

Add one complete pin set whose handover points outside its own caller-cell
rectangle and one pair whose tunnels overlap. Assert the exact new
`PlannerError::InvalidPortPin` payload. Add the same outside-facing pin as a
partial set and assert it still reaches the existing validation result rather
than the new footprint refusal.

- [ ] **Step 5: Verify RED**

```powershell
cargo test --lib complete_pins_refuse_a_handover_outside_their_footprint
```

Expected: the complete case is accepted or returns an older refusal instead of
`OutsideIoFootprint`.

- [ ] **Step 6: Implement bounded-only pin validation**

In `validate_port_placements`, preserve the current validation order. After
the existing vertical/world/gap/collision checks, derive `IoFootprint` only
when every declared port is present. Then:

```rust
for (port, pin, role) in &roles {
    for cell in [pin.handover(*role), pin.net_cell(*role)] {
        if !footprint.contains_xz(cell) {
            return Err(invalid(port, pin, PinRefusal::OutsideIoFootprint { cell }));
        }
    }
}
```

Compare each pair's tunnel sets in stable role order and return
`ClearanceConflict` against the first overlapping cell. Partial sets never
enter this branch.

- [ ] **Step 7: Verify GREEN and legacy tests**

Run serially:

```powershell
cargo test --lib compile::planner::tests
cargo test --test terminal_handover
```

- [ ] **Step 8: Commit**

```powershell
git add src/compile/planner.rs
git commit -m "feat(synthesis): derive bounded IO footprint and terminal tunnels"
```

---

### Task 3: Feed complete bounds into the existing 2D placer

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs:56-189,305-812,1078-1185`
- Modify: `src/compile/fragment_synth/api.rs:200-265,518-555`
- Test: `src/compile/fragment_synth/placement.rs` tests near `2285-2374,2877-2905`

**Interfaces:**
- `SeedPlacementError` gains `DegenerateIoFootprint { footprint: IoFootprint }`.
- `SeedPlacementPlan` gains `pub(crate) io_footprint: Option<IoFootprint>`.
- Produces:
  ```rust
  fn io_footprint(request: SeedPlacementRequest<'_>) -> Option<IoFootprint>;
  fn lateral_window(
      frame: PlacementFrame,
      pins: &BTreeMap<PhysicalEndpointId, PortPin>,
      footprint: Option<IoFootprint>,
  ) -> LateralWindow;
  fn forward_limit(
      frame: PlacementFrame,
      pins: &BTreeMap<PhysicalEndpointId, PortPin>,
      footprint: Option<IoFootprint>,
  ) -> Option<i32>;
  ```

- [ ] **Step 1: Add failing bounded-plan tests**

Use the existing small graph helpers. Assert:

```rust
assert_eq!(plan.io_footprint, Some(IoFootprint { min_x: 10, max_x: 50, min_z: 20, max_z: 60 }));
assert_eq!(plan.window.width(), Some(41));
assert!(matches!(
    TopologyAwareSeedPlacer.plan(degenerate_request),
    Err(SeedPlacementError::DegenerateIoFootprint { .. })
));
```

Add a partial-pin request and assert `plan.io_footprint.is_none()` plus the
existing no-block fingerprint literal.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib complete_pins_close_both_placement_axes
```

- [ ] **Step 3: Implement closed limits through existing helpers**

Derive complete footprint from the endpoint map and expected graph port count.
Reject equal min/max on either axis. When present, project its four corners;
return both lateral bounds from `lateral_window` and the projected forward max
from `forward_limit`. Keep current bodies unchanged under `None`.

Bounded-only spacing extends private `derive_channel_widths` to return the
last channel's lane count beside the widths. That count comes from the exact
crossing intervals already passed to `lane_count`; do not infer it back from a
rounded width. Then use:

```rust
let turnaround = bounded_turnaround_allowance(last_lane_count);
```

Legacy mode continues calling `legacy_turnaround_allowance()` and retains
`LATERAL_GAP`, `WINDOW_MARGIN`, and endpoint margins unchanged. Keep the
`- ROW_GRID` fold budget guard.

- [ ] **Step 4: Add and implement the bounded-only case marker**

Write a failing API test asserting unpinned and partial cases keep the old
case fingerprint while a complete case differs when the bounded marker is
toggled. Add `b"io-bounded-3d-v1"` to `CaseDescriptor` serialization only when
the same complete-footprint predicate is true; do not change
`topology_aware_seed_placement_revision()`.

- [ ] **Step 5: Verify GREEN and fingerprint guards**

```powershell
cargo test --lib complete_pins_close_both_placement_axes
cargo test --lib placement_revision_changes_only_the_case_fingerprint
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
cargo test --test fragment_synth_architecture
```

- [ ] **Step 6: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/api.rs
git commit -m "feat(synthesis): close placement bounds for complete IO"
```

---

### Task 4: Enforce the footprint at every movement and at finalization

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs:900-960`
- Modify: `src/compile/fragment_synth/seed.rs:80-91,1131-1265,1374-1730`
- Modify: `src/compile/fragment_synth/candidate.rs:193-250`
- Modify: `src/compile/fragment_synth/seed.rs:776-800`
- Test: `src/compile/fragment_synth/seed.rs` tests near `3135-3225,5402-5470`
- Test: `src/compile/fragment_synth/candidate.rs` tests near `1697-1790`

**Interfaces:**
- `CandidateError` gains `IoFootprintViolation { at: Anchor }`.
- Produces:
  ```rust
  fn blocks_fit_footprint(blocks: &[PlacedBlock], footprint: Option<IoFootprint>) -> bool;
  impl ExpandedPhysicalCandidate {
      pub(crate) fn validate_io_footprint(&self, netlist: &Netlist)
          -> Result<(), CandidateError>;
  }
  ```

- [ ] **Step 1: Add failing movement tests**

Add four focused tests: an instance override, block offset, primitive shell
candidate, and `move_owner` repair that each attempt to cross one boundary.
Assert the first two/shell end through existing `PlacementExhausted` or the
new bounded placement refusal; assert repair returns existing
`ImmovableRepairOwner`. Repeat one override with partial pins and assert the
legacy move is still allowed.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib bounded_post_plan_moves_never_cross_the_io_footprint
```

- [ ] **Step 3: Implement one containment predicate at all four sites**

At seed sites, check the exact `PlacedBlock` list already built; do not rebuild
an envelope. In placement repair, use the oriented bounds already in the
plan-in-frame local map. Filter shell candidates before committing them. Do
not add `dy` to `InstancePlacementOverride` or `BlockPlacementOffset`—both
remain horizontal optimization controls.

- [ ] **Step 4: Add the failing exhaustive invariant tests**

Construct a candidate with complete pins and one owned placement at
`x = max_x + 1`; assert exactly
`CandidateError::IoFootprintViolation { at }`. The same candidate with a
partial pin set must return `Ok(())`. Add a hierarchical union case to prove
the union preserves `pins`/`pin_contracts` and the final check sees a stamped
block after translation.

- [ ] **Step 5: Verify RED**

```powershell
cargo test --lib final_complete_pin_footprint_walk_rejects_one_escaped_anchor
```

- [ ] **Step 6: Implement the final check once**

Inside `finish_attempt`, after shape/ownership validation and before emission:

```rust
candidate.validate_io_footprint(input.lowered)?;
```

The method derives completeness from `self.pin_contracts` and the netlist,
then calls the existing `relocate::anchors_of(self)`. It returns on the first
out-of-range anchor in that walk. Do not add a second anchor walker.

- [ ] **Step 7: Verify GREEN**

```powershell
cargo test --lib bounded_post_plan_moves_never_cross_the_io_footprint
cargo test --lib final_complete_pin_footprint_walk_rejects_one_escaped_anchor
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

- [ ] **Step 8: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs src/compile/fragment_synth/candidate.rs
git commit -m "feat(synthesis): enforce bounded placement containment"
```

---

### Task 5: Reject and reserve terminal-tunnel encroachment

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs:629-770,802-925,2805-2865`
- Test: `src/compile/fragment_synth/seed.rs` tests near `3781-3875,4227-4265`

**Interfaces:**
- Produces:
  ```rust
  fn reserve_terminal_tunnels(
      candidate: &ExpandedPhysicalCandidate,
      netlist: &Netlist,
      reservations: &mut PhysicalReservations,
  ) -> Result<(), SeedError>;
  ```
- Macro overlap returns `SeedError::InvalidPins(PlannerError::InvalidPortPin {
  refusal: PinRefusal::ClearanceConflict { .. }, .. })`.

- [ ] **Step 1: Add failing complete/partial tunnel tests**

Create a tiny complete-pin fixture whose forced primitive body occupies one
non-exempt tunnel cell. Assert the named port/refusal/cell. Create the same
geometry with one missing pin and assert it keeps today's five-neighbour
signal-only contract. Add a route-floor probe proving a floor cannot occupy a
reserved tunnel cell.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib a_complete_pin_tunnel_rejects_macro_and_route_floor_cells
```

- [ ] **Step 3: Implement placement-time rejection and typed reservation**

Run after `place_boundaries`, `place_blocks`, and `place_instances`, but before
`route_all`. For each complete pin, intersect `terminal_tunnel` with footprint
and `y >= 0`; exempt only `H` and `H.down()` as required terminal hardware.
`N` is outside the tunnel. Require `C` to remain air. If an
owned component occupies another cell, return the typed pin refusal. Otherwise
reserve each cell as endpoint-owned `KeepOut`; use `MandatoryAir` where a
route floor would otherwise be legal.

- [ ] **Step 4: Verify GREEN and old terminal contract**

```powershell
cargo test --lib a_complete_pin_tunnel_rejects_macro_and_route_floor_cells
cargo test --test terminal_handover
cargo test --test build_circuit_pins a_pin_set_built_without_the_parser_meets_the_same_door
```

- [ ] **Step 5: Commit**

```powershell
git add src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): reserve complete-pin terminal tunnels"
```

---

### Task 6: Add height-aware envelopes and deterministic deck packing

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs:31-160,220-305,345-705,1669-1740`
- Modify: `src/compile/fragment_synth/seed.rs:270-325`
- Test: `src/compile/fragment_synth/placement.rs` test module

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
  pub(crate) struct DeckId(pub u32);

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) struct DeckPlan {
      pub ground: i32,
      pub min_y: i32,
      pub max_y: i32,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
  pub(crate) struct FloorplanMetrics {
      pub macro_volume: u64,
      pub union_volume: u64,
      pub deck_count: u32,
      pub cross_deck_nets: u32,
      pub vertical_trunk_lanes: u32,
  }
  ```
- `NodeFacts` gains `pub deck: DeckId` without changing `forward_level`.
- `BlockFacts` gains `height: i32` from existing `BlockBounds.min/max.y`.
- `MacroBounds` gains `min_y/max_y`; `SeedPlacementPlan` gains ordered
  `decks: BTreeMap<DeckId, DeckPlan>` and `floorplan: FloorplanMetrics`.
- `SeedPlacementError` gains the minimal unit variant
  `NoDeckLayoutFits`; the error text is `no ordered deck layout fits the IO footprint`.
- Private `DeckColumn { owner, lead, cost, close }` carries only the values
  required by the shelf pack. `pack_decks(columns, capacity)` returns one
  `DeckId` per ordered column; `deck_grounds(base, locals)` returns absolute
  `DeckPlan` intervals.
- `derive_channel_widths` returns the existing width map plus the lane-count
  map from the same `lane_count` calls already made for each channel level.

- [ ] **Step 1: Add failing height tests**

Use one known primitive variant and one literal block bound. Assert the
envelope's hand-derived min/max Y and block height. Verify a block with an
invalid Y span is refused through the existing block-resolution error path.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib macro_envelopes_keep_their_real_vertical_bounds
```

- [ ] **Step 3: Implement vertical bounds**

While `macro_envelope` already walks every variant block/port, include `y` in
the bounds. In `ResolvedBlocks::resolve`, call the existing `span` closure for
`compiled.bounds.min.y/max.y`. Deck zero remains `frame.origin.y`.

- [ ] **Step 4: Add failing pure deck-pack tests**

Test literal ordered columns against a literal forward capacity:

```rust
let col = |owner, lead, cost| DeckColumn {
    owner: InstanceId(owner), lead, cost, close: 0,
};
let columns = [col(0, 0, 6), col(1, 0, 6), col(2, 0, 4)];
assert_eq!(pack_decks(&columns, 10).unwrap(), vec![DeckId(0), DeckId(1), DeckId(1)]);
let closing = [
    DeckColumn { close: 3, ..col(0, 0, 6) },
    DeckColumn { close: 3, ..col(1, 2, 4) },
];
assert_eq!(pack_decks(&closing, 9).unwrap(), vec![DeckId(0), DeckId(1)]);
```

Also assert one width-11 column returns `LateralWindowTooNarrow`, deck order
never moves a later column backward, and the same inputs produce equal output.
Add a pure `deck_grounds` check proving two local `(-1, 3)` reservation
intervals at base ground 1 become absolute intervals `0..=4` and `5..=9`.

- [ ] **Step 5: Verify RED**

```powershell
cargo test --lib ordered_shelf_pack_uses_the_minimum_stable_decks
```

- [ ] **Step 6: Add the failing real-plan and metric test**

Add `bounded_columns_fold_onto_ordered_decks` using a three-level NOR chain
with a complete non-degenerate footprint whose projected forward capacity is
48 cells. Hand-derive one-lane channel widths and assert levels `0,1` on deck
0 and level `2` on deck 1. Assert both absolute deck intervals, all three
forward origins, `deck_count == 2`, `macro_volume == 6`, and
`union_volume == 84`. Call the real placer, not `pack_decks` directly.

- [ ] **Step 7: Verify real-plan RED**

```powershell
cargo test --lib bounded_columns_fold_onto_ordered_decks
```

- [ ] **Step 8: Implement order-preserving shelf packing and deck grounds**

Pack consecutive columns greedily into the complete footprint's usable
forward span. In bounded mode every deck reuses the same projected
`start = cursor` and `capacity = forward_limit - start`. A column's `cost` is
its macro forward span plus its following channel. Deck 0 starts with
`lead = 0`; the first column of every upper deck pays its preceding channel
width as `lead`. Charge turnaround once per deck from that deck's closing
level lane count. Append a column iff lead plus all costs plus the candidate
closing turnaround fits; otherwise open the next deck. A singleton that does
not fit returns `LateralWindowTooNarrow { instance }`; non-positive capacity
returns `NoDeckLayoutFits`. Use `i64` for pack sums. Legacy `None` mode keeps
its existing guards, turnaround, positions, error and fingerprint unchanged.
The pack's inclusive `forward_span + channel` is only its conservative fit
cost; actual origins keep the existing cursor step
`max_forward - min_forward + channel`.

Initialize `NodeFacts.deck` to deck zero in topology analysis, preserve
`forward_level`, then mutate only `deck` once in `plan_in_frame` after column
origins are known. For each
deck derive the local reservation interval:

```rust
let local_min = macro_min_y.min(-1);
let local_max = (macro_max_y + 3).max(3);
next_ground = previous_ground + previous_local_max - next_local_min + 1;
```

Here `next_local_min` is the untranslated lower bound of the deck being placed,
not the previous deck's bound. `macro_max_y + 3` is the existing router ceiling
above the highest local endpoint; `3` is the channel slab top. Use checked arithmetic. Compute
`planned_macro_fill` as the exact rational pair `(macro_volume, union_volume)`;
never compare floats. `macro_volume` is the sum of macro-envelope volumes;
`union_volume` is the bounding-box volume of the deck-translated macro
envelopes in `(forward, lateral, y)`. Use each macro's Y envelope at its deck
ground, not the wider `DeckPlan` reservation interval. `DeckPlan.min_y/max_y`
are absolute. Empty plans report zero volumes, one base deck, and zero
cross-deck/trunk counts; their deck map contains `DeckId(0)` with ground
`frame.origin.y` and absolute reservation interval `ground - 1..=ground + 3`.

Update `complete_pins_close_both_placement_axes` for the new bounded refusal
that replaces its pre-deck `NoFrameFits`, with the arithmetic recorded in the
test. Do not change legacy error text or unbounded frame selection.

- [ ] **Step 9: Verify GREEN and legacy plan identity**

```powershell
cargo test --lib macro_envelopes_keep_their_real_vertical_bounds
cargo test --lib ordered_shelf_pack_uses_the_minimum_stable_decks
cargo test --lib bounded_columns_fold_onto_ordered_decks
cargo test --lib complete_pins_close_both_placement_axes
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

- [ ] **Step 10: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): plan height-aware macro decks"
```

---

### Task 7: Materialize primitive and block decks in Y

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs:687-705,1296-1304`
- Modify: `src/compile/fragment_synth/seed.rs:1131-1265,1374-1730`
- Test: `src/compile/fragment_synth/seed.rs` tests near `5312-5470`

**Interfaces:**
- Existing `PreferredInstancePose.preferred_origin.y` carries the deck ground.
- Existing `relocate::Offset.dy` carries block translation.
- `InstancePlacementOverride` and `BlockPlacementOffset` stay 2D.

- [ ] **Step 1: Add failing primitive/block Y tests**

Create one two-deck primitive fixture and one two-block parent fixture. Assert
the upper primitive's anchor equals its planned Y. For the block fixture assert:

```rust
assert_eq!(upper_offset.dy, (upper_pose.preferred_origin.y - 1) - compiled.bounds.min.y);
assert_eq!(lower_offset.dy, (lower_pose.preferred_origin.y - 1) - compiled.bounds.min.y);
assert!(upper_offset.dy > lower_offset.dy);
```

Also assert a horizontal block optimization changes only `dx/dz`, not the deck
selected by the placer.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib block_placement_uses_its_planned_deck_y
```

Expected: both blocks still receive the old common-frame Y.

- [ ] **Step 3: Implement minimal Y propagation**

Add a Y argument to the private `frame_to_world` helper and pass each deck's
ground when constructing `PreferredInstancePose`. In `place_blocks`, replace
the old frame-origin expression with:

```rust
dy: (pose.preferred_origin.y - 1) - compiled.bounds.min.y,
```

Primitive placement already preserves `preferred_origin.y` through horizontal
overrides and shell search; do not add another Y field. The `-1` preserves the
existing contract that a block's included floor row sits one below the deck's
body/channel ground.

- [ ] **Step 4: Add the minimal physical deck-separation test**

Build two literal component reservation sets, translate the upper set to the
derived ground, and assert they are disjoint including `MandatoryAir`. Add one
intended same-net staircase through the reserved trunk corridor and assert the
simulator sees connectivity while an adjacent unrelated probe stays low.

- [ ] **Step 5: Verify GREEN**

```powershell
cargo test --lib block_placement_uses_its_planned_deck_y
cargo test --lib derived_deck_spacing_separates_unrelated_cells_but_keeps_the_trunk
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

- [ ] **Step 6: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): materialize macro decks in three dimensions"
```

---

### Task 8: Make channel layout deck-local and mergeable

**Files:**
- Modify: `src/compile/fragment_synth/channel_layout.rs:42-101,189-360,1050-1485`
- Modify: `src/compile/fragment_synth/placement.rs:177-196`
- Modify: `src/compile/fragment_synth/seed.rs:392-465,2293-2665`
- Test: `src/compile/fragment_synth/channel_layout.rs` test module

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone)]
  pub(crate) struct DeckSinkGeometry {
      pub endpoint: PhysicalEndpointId,
      pub geometry: TargetGeometry,
      pub level: i64,
      pub synthetic_trunk: bool,
  }

  #[derive(Debug, Clone)]
  pub(crate) struct DeckNetGeometry {
      pub owner: PhysicalEndpointId,
      pub source: SourceGeometry,
      pub source_level: i64,
      pub source_is_synthetic_trunk: bool,
      pub sinks: Vec<DeckSinkGeometry>,
  }

  pub(crate) fn plan_deck_channel_layout(
      candidate: &ExpandedPhysicalCandidate,
      analysis: &SeedPlacementAnalysis,
      deck: DeckId,
      ground: i32,
      members: &BTreeSet<InstanceId>,
      include_boundaries: bool,
      footprint: Option<IoFootprint>,
      nets: &[DeckNetGeometry],
      router: &dyn PhysicalRouter,
      reservations: &mut PhysicalReservations,
      limits: RouterLimits,
  ) -> Result<ChannelLayout, ChannelLayoutError>;
  ```
- `ChannelLayout.lanes` becomes
  `BTreeMap<(DeckId, usize), BTreeMap<PhysicalEndpointId, i32>>`.
- `LayoutRepair` gains bounded-only
  `WidenDeckChannel { deck: DeckId, level: i64, width: i32 }`.
- `ChannelLayoutError` appends bounded-only
  `DeckChannelTooNarrow { deck: DeckId, channel: usize, level: i64,
  available: i32, lanes: usize, needed: i32 }`; the existing
  `ChannelTooNarrow` variant and its legacy construction stay unchanged.
- The existing `plan_channel_layout` remains a legacy wrapper that passes one
  synthetic deck plus `None` and returns byte-identical cells. Bounded callers
  pass `Some(footprint)`; no `i32::MIN/MAX` sentinel is introduced.

- [ ] **Step 1: Add failing single/two-deck tests**

First assert the legacy wrapper produces exactly the existing layout for a
small fixture. Then call `plan_deck_channel_layout` twice with equal local
levels but different grounds and assert:

```rust
assert!(lower.closed.iter().all(|at| at.y <= lower_ground + 3));
assert!(upper.closed.iter().all(|at| at.y >= upper_ground));
assert!(lower.closed.is_disjoint(&upper.closed));
assert!(lower.lanes.contains_key(&(DeckId(0), 0)));
assert!(upper.lanes.contains_key(&(DeckId(1), 0)));
```

Add `bounded_channel_widening_is_keyed_by_deck_and_level`: make the bounded
kernel return a too-narrow result and assert the retry records and monotonically
grows `WidenDeckChannel` for that exact `(deck, level)`, while the equivalent
legacy call still records `WidenChannel`. Then report the same stable global
level from a different deck and assert there is still one canonical repair: its
deck changes to the current deck while its width keeps growing.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib equal_levels_on_two_decks_never_alias_channel_state
cargo test --lib bounded_channel_widening_is_keyed_by_deck_and_level
```

- [ ] **Step 3: Refactor the existing planner into one deck-local kernel**

Replace endpoint-to-level inference with the explicit levels in
`DeckNetGeometry`. Preserve every real sink's `PhysicalEndpointId` for current
diagnostics and route construction; synthetic trunk endpoints carry the net
owner plus their explicit boolean and never enter the final real-sink request.
Keep `analysis` as the source of each member instance's forward level when
building columns, and filter candidate primitives/junctions/blocks to
`members`. Include both physical-boundary reads (column occupancy and escape
corridor exclusion) only for deck zero. Only under `Some(footprint)`, clamp the
closed slab's forward/lateral loops to the projected footprint. Keep the old
wrapper's inputs and exact output for unbounded/partial cases.

- [ ] **Step 4: Add deterministic layout union**

Implement:

```rust
impl ChannelLayout {
    fn merge(&mut self, other: ChannelLayout) {
        self.closed.extend(other.closed);
        for (owner, cells) in other.private {
            self.private.entry(owner).or_default().extend(cells);
        }
        for (owner, floors) in other.floors {
            self.floors.entry(owner).or_default().extend(floors);
        }
        for (owner, cells) in other.departures {
            self.departures.entry(owner).or_default().extend(cells);
        }
        for (key, lanes) in other.lanes {
            assert!(self.lanes.insert(key, lanes).is_none(), "duplicate deck channel");
        }
    }
}
```

The lane insertion assertion rejects duplicate `(deck, channel)` keys instead
of silently overwriting them.

- [ ] **Step 5: Key bounded widening by deck and level**

Emit `DeckChannelTooNarrow` only when the kernel receives `Some(footprint)` and
route it into `WidenDeckChannel`; preserve the legacy `ChannelTooNarrow` to
`WidenChannel` branch exactly. Append both new enum variants after existing
variants so legacy ordering remains unchanged.

Keep `plan_with_widths` level-keyed: the analysis global forward level is stable
and each level produces exactly one packed column. For a bounded error, find the
prior `WidenDeckChannel` by global `level`, grow from that width, remove that one
entry, and insert the current `(deck, level, width)`. Thus repacking may update
the diagnostic deck without resetting progress. The placer consumes both
repair variants into the same stable level-keyed minimum-width map; bounded
repairs are never created for `None` footprints. On the bounded retry cap,
preserve the last real `DeckChannelTooNarrow` as the refusal. The legacy cap
branch and its exact synthetic `ChannelTooNarrow` remain untouched.

- [ ] **Step 6: Verify GREEN**

```powershell
cargo test --lib equal_levels_on_two_decks_never_alias_channel_state
cargo test --lib bounded_channel_widening_is_keyed_by_deck_and_level
cargo test --lib compile::fragment_synth::channel_layout::tests
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

- [ ] **Step 7: Commit**

```powershell
git add src/compile/fragment_synth/channel_layout.rs src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): plan independent channels per deck"
```

---

### Task 9: Allocate vertical trunks and close the footprint perimeter

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs:345-705`
- Modify: `src/compile/fragment_synth/seed.rs:160-220,2293-2805`
- Modify: `src/compile/fragment_synth/channel_layout.rs:89-101`
- Test: `src/compile/fragment_synth/placement.rs` and `seed.rs` test modules

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
  pub(crate) struct VerticalTrunkLane {
      pub owner: PhysicalEndpointId,
      pub lateral: i32,
      pub min_forward: i32,
      pub max_forward: i32,
      pub first_deck: DeckId,
      pub last_deck: DeckId,
  }
  ```
- `SeedPlacementPlan` gains
  `vertical_trunks: BTreeMap<PhysicalEndpointId, VerticalTrunkLane>`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) enum SeedRoutingContext {
      Ordinary,
      VerticalTrunkUnroutable,
  }
  ```
- `SeedRoutingFailure` gains `context: SeedRoutingContext` while retaining its
  existing `category: RouterRefusalCategory`. Existing constructors set
  `Ordinary` and preserve the exact old `Display`; a synthetic trunk endpoint
  failure for a real sink whose source deck differs sets
  `VerticalTrunkUnroutable` and names that context in `Display`.
  Outer `SeedError::Routing` stays unchanged.

- [ ] **Step 1: Add failing monotonic-band tests**

Use a literal graph where the initial pack creates two cross-deck nets and the
reserved band forces one more column onto the upper deck. Assert band widths
observed by a test hook are monotonically `[0, 8, 12]`, final lane owners are
sorted physical source IDs, and repeating the plan is identical. Add a case
whose band consumes the usable lateral span and assert `NoDeckLayoutFits`.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib vertical_trunk_band_only_grows_and_terminates
```

- [ ] **Step 3: Implement the bounded fixed-point loop**

Start band width at zero. Pack, derive unique physical source owners whose
source/sink decks differ, and require one `ROW_GRID` lane per owner. If
`owners.len() * ROW_GRID` exceeds the reserved band, grow and repack. Never
shrink. Place lane centers from the projected lateral maximum inward in owner
order. Stop when the current owners fit.

- [ ] **Step 4: Add failing trunk/perimeter routing tests**

For a two-deck net, assert its three-cell-wide private corridor spans every Y
between deck channel slabs, remains within the footprint, and is absent from
`closed`. Add a router probe whose only apparent path exits one footprint side
and re-enters; expect refusal. Add the same probe with partial pins and expect
the legacy route result. Force a real cross-deck sink past a zero
`NodeExpansions` limit and assert both
`failure.context == SeedRoutingContext::VerticalTrunkUnroutable` and the
underlying `failure.category`; repeat an ordinary route refusal and assert its
context and formatted text remain byte-identical to the existing expectation.

- [ ] **Step 5: Verify RED**

```powershell
cargo test --lib vertical_trunk_is_the_only_open_cross_deck_corridor
```

- [ ] **Step 6: Materialize trunks before deck channel closure**

For each lane, reserve the three lateral rows centered on `lane.lateral`, the
full usable forward interval, and every Y between first/last deck slabs in the
owner's `ChannelLayout.private` set. Build each deck's `DeckNetGeometry` with
the physical endpoints on that deck and a synthetic trunk source/sink at the
lane; the final `route_all` request still contains only real sinks.

Call the deck-local kernels and merge their layouts in ascending `DeckId`, so
the lowest deck's first existing error remains authoritative.

Build one-cell `KeepOut` perimeter cells just outside min/max X/Z from the
lowest route bound through the maximum of all route
`max(source.y, sink.y) + 3` bounds. Omit a side below world zero and use checked
arithmetic. Insert perimeter and trunk reservations before any deck computes
`closed`.

Before `route_all`, derive a `BTreeSet<RoutedSinkId>` for real sinks whose
source deck differs from their sink deck. When mapping a router failure, use
membership in that set to select
`SeedRoutingContext::VerticalTrunkUnroutable`; synthetic channel-layout
endpoints never enter the final route request. Do not add a case to the shared
`RouterRefusalCategory`.

- [ ] **Step 7: Verify GREEN**

```powershell
cargo test --lib vertical_trunk_band_only_grows_and_terminates
cargo test --lib vertical_trunk_is_the_only_open_cross_deck_corridor
cargo test --lib derived_deck_spacing_separates_unrelated_cells_but_keeps_the_trunk
```

- [ ] **Step 8: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs src/compile/fragment_synth/channel_layout.rs
git commit -m "feat(synthesis): route deterministic vertical trunks"
```

---

### Task 10: Certify forced primitive and hierarchical multi-deck fixtures

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs` test module
- Modify: `tests/hierarchical_synthesis.rs`
- Modify: `.superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md`

**Interfaces:**
- Consumes all bounded floorplan interfaces.
- Produces no new production API.

- [ ] **Step 1: Add failing primitive integration tests**

Build a small complete-pin flat fixture whose footprint is wide enough for one
column but not all ordered columns. Assert it certifies at budget zero, uses at
least two decks, and has one non-terminal macro-owned cell at
`y >= base_ground + 4`. Widen only its pin rectangle and assert the same
netlist uses deck zero only and certifies. Add the non-degenerate narrow
two-pin case from the spec.

- [ ] **Step 2: Verify RED**

```powershell
cargo test --lib a_complete_narrow_fixture_certifies_on_two_real_decks
```

- [ ] **Step 3: Fix only general placement/routing rules exposed by RED**

Use systematic debugging for any failure. Changes must apply to the shared
bounded path; no fixture-name, instance-ID, or seven-segment special case is
allowed. Re-run the focused test after each minimal fix.

- [ ] **Step 4: Add failing hierarchical integration test**

In `tests/hierarchical_synthesis.rs`, create a complete-pin parent with two
instances of one compiled child. Pins must derive an East-facing frame. Assert
both stamps share one compiled module, occupy different decks, flatten into one
candidate, and certify.

- [ ] **Step 5: Verify RED then GREEN**

```powershell
cargo test --test hierarchical_synthesis bounded_parent_stamps_one_module_on_two_decks
```

Expected before fixes: failure in block Y/channel/trunk behavior. After only
general fixes: pass.

- [ ] **Step 6: Run task regression set**

```powershell
cargo test --lib a_complete_narrow_fixture_certifies_on_two_real_decks
cargo test --test hierarchical_synthesis
cargo test --lib no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly
```

- [ ] **Step 7: Record and commit**

Record deck count, used height, planned macro fill, ticks, blocks, and wall time
for both fixtures in the progress ledger.

```powershell
git add src/compile/fragment_synth tests/hierarchical_synthesis.rs .superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md
git commit -m "test(synthesis): certify flat and hierarchical 3D decks"
```

---

### Task 11: Run the bounded pinned seven-segment trial

**Files:**
- Modify: `tests/build_circuit_pins.rs:312-861` (tests only; never pin literals)
- Modify: `src/compile/fragment_synth/benchmark.rs` only if a report field is
  required; do not change existing replacement acceptance semantics
- Modify: `tests/fixtures/fragment_synth_baseline.json`
- Modify: `tests/fixtures/fragment_synth_shipping.json`
- Modify: `.superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md`

**Interfaces:**
- Produces no public API.
- The plan-only density comparison uses exact cross multiplication:
  `new_macro * old_union > old_macro * new_union` with checked `u128`.

- [ ] **Step 1: Run the seven-segment plan-only preflight before routing**

Use one temporary, uncommitted test stop immediately after bounded placement,
terminal-tunnel reservation, deck assignment, and vertical-trunk allocation,
but immediately before the first `plan_deck_channel_layout`/`route_all` call.
The stop prints `PLAN_ONLY_OK` plus footprint, macro count, deck count,
cross-deck net count, trunk lanes, and `(macro_volume, union_volume)`, then
panics so no router call can begin. Run:

```powershell
cargo test --test build_circuit_pins topology_aware_seed_preserves_the_checked_seven_segment_pin_contract -- --nocapture
```

Proceed only when the observed terminal line is the intentional
`PLAN_ONLY_OK` stop after every macro and trunk has a legal placement. If
placement returns any typed error first, fix that shared plan rule and rerun;
do not start the routing trial. Remove the temporary stop with `apply_patch`,
then verify `rg -n "PLAN_ONLY_OK" src tests` returns no matches. This is a
one-shot acceptance probe, not a public API or committed environment switch.

- [ ] **Step 2: Add the failing bounded seven-segment assertions**

Extend the existing checked fixture test without changing
`checked_seven_segment_pin_contract()`. Assert:

- compilation now succeeds rather than returning the committed queue refusal;
- all eleven literal caller/handover/net-cell tuples are unchanged;
- every `relocate::anchors_of` X/Z lies inside `68..=112`, `24..=120`;
- all sixteen input masks produce the expected seven outputs;
- if Stage 0 reports multiple decks, at least one non-terminal macro cell is at
  `base_ground + 4` or above; otherwise the forced fixture from Task 10 owns
  that proof;
- bounded `planned_macro_fill` strictly exceeds the legacy plan-only ratio.

- [ ] **Step 3: Verify RED**

```powershell
cargo test --test build_circuit_pins topology_aware_seed_preserves_the_checked_seven_segment_pin_contract -- --nocapture
```

Expected: the current pinned route refuses or a new bounded assertion fails.

- [ ] **Step 4: Run the bounded implementation loop**

Use systematic debugging on the first typed refusal. Fix shared geometry,
reservation, or routing logic only. Do not raise caps, move pins, widen the
footprint, or add a case-specific branch. Re-run the one test until GREEN.

- [ ] **Step 5: Capture plan/final metrics**

Record legacy and bounded `planned_macro_fill` numerator/denominator, final
blocks, occupied volume, physical fill ratio, observed settle, static delay,
used height, deck count, cross-deck net count, trunk lanes, and wall time.
Physical fill becomes the new baseline because the committed pinned case had
none.

- [ ] **Step 6: Regenerate fixtures through scratch paths**

Use fresh paths; never overwrite tracked files directly:

```powershell
$trialDir = Join-Path $env:TEMP ("reda-io3d-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $trialDir | Out-Null
cargo run --release --bin fragment_baseline -- --output (Join-Path $trialDir 'baseline.json') --replace
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output (Join-Path $trialDir 'shipping.json') --shipping-source (Join-Path $trialDir 'shipping.rs') --shuffle-seed 0x5245444120260831
```

Inspect and copy only the intended baseline/shipping JSON changes with
`Copy-Item -LiteralPath`; do not copy generated shipping Rust.

- [ ] **Step 7: Verify fixture contracts**

```powershell
cargo test --test fragment_synth_baseline
cargo test --test fragment_synth_acceptance
cargo test --test build_circuit_pins
```

- [ ] **Step 8: Commit**

```powershell
git add tests/build_circuit_pins.rs tests/fixtures/fragment_synth_baseline.json tests/fixtures/fragment_synth_shipping.json .superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md
git commit -m "feat(synthesis): certify bounded pinned seven segment"
```

---

### Task 12: Determinism, full verification, and acceptance report

**Files:**
- Modify: `src/compile/fragment_synth/certification.rs:2126-2245`
- Modify: `src/compile/fragment_synth/seed.rs:5090-5257`
- Modify: `.superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md`

**Interfaces:**
- No production API changes.
- Existing success/refusal comparison helpers gain bounded fixtures.

- [ ] **Step 1: Add failing bounded worker-matrix cases**

Add the small bounded success fixture to
`certified_candidate_is_identical_at_one_two_and_four_workers` and a bounded
vertical-trunk refusal to
`exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count`.
Compare candidate/world/equivalence/timing/manifest/measurements/metrics and
the exact lowest refusal at 1/2/4 workers. Add the bounded hierarchical fixture
to the existing `assert_matrix_agrees` path so proposal traces and terminal
work are non-empty.

- [ ] **Step 2: Verify RED then GREEN**

```powershell
cargo test --lib certified_candidate_is_identical_at_one_two_and_four_workers
cargo test --lib exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count
```

If RED exposes nondeterminism, sort at the proposal/frontier creation point;
never sort only the final report.

- [ ] **Step 3: Run focused debug verification serially**

```powershell
cargo test --lib compile::planner::tests
cargo test --lib compile::fragment_synth::placement::tests
cargo test --lib compile::fragment_synth::channel_layout::tests
cargo test --lib compile::fragment_synth::seed::tests
cargo test --lib compile::fragment_synth::certification::tests
cargo test --test terminal_handover
cargo test --test fragment_synth_architecture
cargo test --test build_circuit_pins
cargo test --test hierarchical_synthesis
cargo test --test fragment_synth_baseline
cargo test --test fragment_synth_acceptance
```

- [ ] **Step 4: Run release acceptance serially**

```powershell
cargo build --release
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture
cargo test --release --lib certification_thread_counts -- --ignored --nocapture --test-threads=1
```

Do not run these concurrently. If a command is interrupted, resume only the
unfinished case with `REDA_EXTRA_CIRCUITS`.

- [ ] **Step 5: Run clippy and diff hygiene**

```powershell
cargo clippy --lib --tests
git diff --check
git status --short
```

Expected: no new warning/error, no whitespace errors, only intended files.

- [ ] **Step 6: Complete the requirement-by-requirement report**

In `progress.md`, include a table with evidence for: complete footprint,
degenerate refusal, partial/unpinned identity, terminal tunnels, post-plan
clipping, final anchor walk, primitive decks, block decks, compile-once/stamp-
many, per-deck channels, vertical trunks, no perimeter escape, seven-segment
16/16 behavior, density ratio, tick/block/volume/height/deck metrics, and
worker 1/2/4 equality. Name every command and exit status.

If tick/static delay is materially worse, keep the bounded path experimental
and report the measured tradeoff for user decision; correctness and bounds do
not fail solely because required stacking costs ticks.

- [ ] **Step 7: Final review and commit**

Request one correctness reviewer and one Ponytail/minimality reviewer over the
full implementation range. Fix every Critical/Important finding and rerun the
affected command before committing:

```powershell
git add src tests docs .superpowers/sdd/2026-09-12-io-bounded-3d-floorplan/progress.md
git commit -m "docs(synthesis): record IO-bounded 3D acceptance"
```
