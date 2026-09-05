# Task 1: Block placement override

## Changed files

- `src/compile/fragment_synth/seed.rs`
  - Added the `block_placements: &BTreeMap<InstanceId, Offset>` parameter to
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

- The block map uses the existing `relocate::Offset`, so it cannot carry the
  irrelevant gate-facing choice. Gate placement overrides remain a separate
  `InstancePlacementOverride` map.
- The selected origin changes before body occupancy, source registration, and
  target registration; the regression asserts selected-body movement, an
  unchanged peer body, and every selected and unselected input/output port.
- Flat compilation and existing hierarchical callers pass empty block maps, so
  their prior gate-placement behaviour stays unchanged.
- The regression setup is intentionally not shortened: both independently
  routed blocks are needed to prove that the selected block moves while the
  peer and all of both blocks' registered ports retain their correct offsets.
- `Offset.dy` is intentionally zero at all current non-empty call sites and
  is not applied; a future vertical block-placement feature must either make
  that contract explicit or support it end-to-end.
