# Task 1: Block placement override

## Changed files

- `src/compile/fragment_synth/seed.rs`
  - Added the `block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>` parameter to
    parent planning and threaded it through `SparseSeedBuilder::plan_attempt`
    into `place_blocks`.
  - Applied a selected block's `dx`/`dz` before its body occupancy claim and
    its input/output route endpoint registration.
  - Flat `build_attempt` supplies an empty block map.
  - Added `block_placement_override_moves_only_the_selected_block_body_and_ports`.
- `src/compile/fragment_synth/hierarchy_api.rs`
  - Existing hierarchical caller supplies an empty block map.
- `src/compile/fragment_synth/union.rs`
  - Existing parent-planning test callers supply an empty block map.

## RED

Command run by the controller:

```text
cargo test --lib block_placement_override_moves_only_the_selected_block_body_and_ports -- --nocapture
```

The test compiled successfully, then failed at `seed.rs:5131`: the selected
block body remained at its baseline coordinates. 955 tests were filtered;
runtime was 0.45s. The cause was that the existing gate placement map was read
only by `place_instances`; `place_blocks` never received or applied a block
offset.

## GREEN

Commands run by the controller:

```text
cargo test --lib block_placement_override_moves_only_the_selected_block_body_and_ports -- --nocapture
# PASS: 1 passed, 0 failed, 955 filtered, 0.43s

cargo test --lib a_parent_routes_into_and_out_of_a_block_with_repeaters_at_the_boundary -- --nocapture
# PASS: 1 passed, 0 failed, 955 filtered, 18.07s
```

The only warning in both runs was the pre-existing dead-code warning for
`BlockFacts.delay_ticks`.

## Self-review

- The block map uses `BlockPlacementOffset { dx, dz }`, so it cannot carry the
  irrelevant gate-facing choice or imply unsupported vertical relocation. Gate
  placement overrides remain a separate `InstancePlacementOverride` map, and
  realised `PlannedParent.block_offsets` remain three-dimensional `Offset`s.
- The selected origin changes before body occupancy, source registration, and
  target registration; the regression asserts selected-body movement, an
  unchanged peer body, and every selected and unselected input/output port.
- Flat compilation and existing hierarchical callers pass empty block maps, so
  their prior gate-placement behaviour stays unchanged.
- The regression setup is intentionally not shortened: both independently
  routed blocks are needed to prove that the selected block moves while the
  peer and all of both blocks' registered ports retain their correct offsets.
- Block placement applies its two supported axes before its occupancy and route
  endpoint registration.

## Fix Round 1

Review found that the initial implementation accepted a three-dimensional
`Offset` but ignored `dy`. An attempted `dy` fix failed before assertions with
`Routing(... category: NoLocalRoute, source_at y=2, sink_at y=1 ...)`: vertical
block relocation is outside the floorplan/router contract. The separate block
map is therefore narrowed to `BlockPlacementOffset { dx, dz }`; realised
`PlannedParent.block_offsets` stay as the existing three-dimensional `Offset`.
The regression again uses `dx: 4, dz: -3`, checking the selected body and every
selected port while its peer remains unchanged.

Corrected 2D patch GREEN evidence from the controller:

```text
block_placement_override_moves_only_the_selected_block_body_and_ports
# PASS: 1 passed, 955 filtered, 0.43s

a_parent_routes_into_and_out_of_a_block_with_repeaters_at_the_boundary
# PASS: 1 passed, 955 filtered, 18.38s
```

The only warning was the same pre-existing `BlockFacts.delay_ticks` dead-code
warning. Cargo was not run in this round.
