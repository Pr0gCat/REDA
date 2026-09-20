# Task 2 Report: Track-aware Layered Placement Plan

## Status

COMPLETE. Task 2 is implemented as a pure placement policy. It consumes the sole Task 1 `analyse_instance_dag` / `SeedPlacementAnalysis`, creates placement-frame coordinates, stable interval tracks, macro envelopes, weighted lane ordering, one forward and one reverse barycentric sweep, deterministic overlap legalization, automatic boundary homes, actual `PhysicalVariant` port-facing scores, embedding-hint penalties, and a canonical complete-plan fingerprint. It does not connect to `SparseSeedBuilder` and does not implement wiring, route schedules, or repair.

## RED evidence

1. Command:

   ```powershell
   cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
   ```

   Result: FAIL during compilation. The first test draft had one wrong `LogicalSignalId` import, and the intended production interfaces were absent: `TopologyAwareSeedPlacer`, `SeedPlacer`, `SeedPlacementRequest`, frame derivation, interval colouring, physical-facing selection, hint scoring, and physical terminal helpers.

2. After correcting only the test import, command:

   ```powershell
   cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
   ```

   Result: FAIL during compilation solely because the new planner/frame/track/physical-port APIs did not exist. This is the authoritative frame/track/physical-port RED run before production implementation.

3. During embedding-hint hardening, command:

   ```powershell
   cargo test --lib compile::fragment_synth::placement::tests::embedding_hint_penalty_changes_the_expected_pose -- --nocapture
   ```

   Result: FAIL during compilation because `choose_hint_aware_facing` did not exist. Minimal facing-plus-hint scoring then made the test green.

4. During refactor from a test-only helper to the real `choose_instance_facing` and real two-torch `Buf` topology, command:

   ```powershell
   cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
   ```

   Result: 12 passed, 1 failed. The fixture did not distinguish East from West, so the real scorer selected literal `CellFacing::WEST` while the test expected `CellFacing::EAST`. The source and target anchors were made asymmetric on X; the hand-derived fixture now uniquely proves no hint selects North and `Coplanar` selects East. No production rule was weakened to satisfy the fixture.

## GREEN evidence

- Initial minimal implementation:

  ```powershell
  cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
  ```

  Result: 13 passed, 0 failed.

- After facing-score, topology-edge, layer-order, pinned-attractor, weighted-median, and envelope refactors:

  ```powershell
  rustfmt --edition 2021 src/compile/fragment_synth/placement.rs
  cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
  cargo test --lib --no-run
  git diff --check
  ```

  Final result: placement tests 13 passed, 0 failed, 821 filtered out; all library tests compiled; formatting and whitespace checks exited 0 with no warnings. The deterministic test constructs two complete plans in one process and compares both the full values and fingerprints.

## Changed files

- `src/compile/fragment_synth/placement.rs`
  - Added the pure Task 2 planner API and plan types.
  - Reused Task 1 `analyse_instance_dag`; no second instance-DAG analysis was added.
  - Added frame, interval-track, physical-port, embedding-hint, forward-level, and complete-plan determinism tests.
- `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/task-2-report.md`
  - This report.

`src/compile/fragment_synth/seed.rs` was not changed because no sibling geometry helper needed broader visibility.

## Commit SHA

Implementation commit: `590edfc9c32d2b90cd2598e097734b0d3a204208` (`feat(synthesis): plan topology-aware seed geometry`).

The report is committed separately so it can contain the immutable implementation SHA without a self-referential commit hash.

## Self-review

- Scope: only pure placement analysis/planning and its tests were changed; there is no `SparseSeedBuilder`, materialisation, route scheduling, routing, or repair integration.
- TDD: tests were written and observed failing before the corresponding production interfaces and hint-aware pose scorer existed.
- Determinism: ordered collections and explicit typed/NESW tie-breaks are used; no randomness or hash iteration was introduced.
- Pins: caller pins are never candidates or outputs of the plan. Their role-specific `net_cell` coordinates set the frame and override matching net lateral attractors; overlap is handled on internal macro origins.
- Tracks: lifetime ordering is span descending, fanout descending, slack ascending, typed signal identity; overlap is inclusive and disjoint intervals reuse the first legal track.
- Geometry: macro envelopes inspect actual selected-topology primitive blocks and ports across all four facings. Facing scoring uses actual input/output terminal world anchors, topology-edge distance, embedding hints, and a forward layer penalty.
- Legalization: columns use per-level maximum envelope width plus a routing channel; same-level instances are stably ordered and shifted laterally until their envelopes do not overlap.
- Fingerprint: covers every preferred instance origin/facing and all automatic input/output homes.
- Formatting: only the touched Rust file was passed to standalone `rustfmt`; `cargo fmt` was not run.

## Concerns

- Task 2 deliberately does not prove seed routing/certification or the six-case budget-zero acceptance gate; Task 3 must inject and materialise this plan before those claims are possible.
- The current physical library exposes no comparator variants. A selected comparator topology therefore returns typed `SeedPlacementError::MissingPhysicalVariant` from the pure planner rather than guessing geometry or panicking.
- Placement constants and scoring weights are deterministic first measurements. Acceptance-quality tuning remains downstream and must not alter pins or router limits.
