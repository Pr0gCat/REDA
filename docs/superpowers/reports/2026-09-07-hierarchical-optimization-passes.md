# Hierarchical Optimization Passes — Retention Report

Date: 2026-09-07

## Result

Retain all three new finite, deterministic hierarchical passes:

1. Block Pull-X
2. Input Seam Absorption
3. Parent Route Repack (safe repeater pruning only)

Every accepted proposal still rebuilds and certifies one flat whole-world
candidate. No pass changes compiled child blocks, pinned IO, `QualityKey`, or
the budget-zero path.

Refresh relocation is not implemented. Direct pruning already produces a
certified real-circuit improvement, so the higher-risk relocation fallback is
deferred until measurements show pruning has stopped paying for itself.

## Certified retention measurements

| Pass | Circuit | Baseline `(settle, blocks, volume, static)` | Candidate | Decision |
|---|---|---:|---:|---|
| Block Pull-X | `ripple_adder8` | `(608, 70603, 1123332, 678)` | `(608, 70601, 1123332, 680)` | Keep: same settle, 2 fewer blocks |
| Input Seam Absorption | `multiplier4` | `(1039, 124948, 3017412, 1070)` | `(1037, 124948, 3017412, 1068)` | Keep: 2 fewer observed ticks |
| Parent Route Repack | `multiplier4` | `(1039, 124948, 3017412, 1070)` | `(1039, 124948, 3017412, 1066)` | Keep: static critical delay reduced by 4 |

The Pull-X result exposes the intentional `QualityKey` ordering: equal settle
then fewer blocks wins before static delay is compared. It is a density/block
count pass on this case, not a latency win. None of these three measurements
reduced occupied bounding-box volume.

The parent-route feasibility screen found 16 changed parent routes and 20
removable repeaters in `multiplier4`. The production stream now prefilters
routes with the same proof, so known-zero-yield routes do not consume a full
proposal compile.

## Verification evidence

- `cargo test --lib compile::fragment_synth::hierarchy_api::tests -- --nocapture`
  - 16 passed, 0 failed, 3 ignored.
- `cargo test --lib compile::fragment_synth::union::tests -- --nocapture`
  - 9 passed, 0 failed before the route-map additions; focused route-map test
    passed after them.
- `cargo test --lib route_opt -- --nocapture`
  - 4 passed, 0 failed.
- `cargo test --release --lib input_seam_absorption_removes_the_child_refresh -- --ignored --nocapture`
  - passed; 2002.85 seconds.
- `cargo test --release --lib parent_route_pruning_improves_multiplier4 -- --ignored --nocapture`
  - passed; 2030.58 seconds.
- `cargo test --release --lib block_pull_x_improves_ripple_adder8 -- --ignored --nocapture`
  - passed; 77.70 seconds.
- Two independent Claude Code reviews ended `APPROVED` after the first seam
  review's findings were fixed. The final pruning review also ended
  `APPROVED` after parent-to-final route-ID mapping, primitive-owned terminal,
  greedy interaction, and stream-stage tests were added.

`cargo clippy --lib --tests -- -D warnings` remains red on the repository's
existing dead-code, large-error, argument-count, type-complexity, and style
warnings. The pass work does not attempt a repository-wide clippy cleanup.

## Deliberate limits

- Parent pruning refuses the first route repeater because source strength is
  not stored on a realised route tree; it only removes a repeater when a
  retained upstream route-owned repeater gives a local strength proof.
- A repeater-to-dust change may create lateral dust adjacency. Full physical
  certification remains the authority and rejects unsafe candidates.
- Density/volume compaction and generation wall-time acceleration with more
  CPU cores or GPU work remain separate follow-up work.
