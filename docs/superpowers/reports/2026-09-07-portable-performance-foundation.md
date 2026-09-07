# Portable Performance Foundation — Baseline and Retention Report

Status: baseline captured; Layer 0 observability measured; later layers and
their retention decisions pending.

## Environment

- Date: 2026-09-07
- Worktree: `topology-aware-seed-v2-6f8f7e`
- HEAD: `f6e4f6d` (`perf: reuse routed hierarchical parent plans`)
- Production prerequisite under measurement: committed incumbent routed-parent
  reuse in `hierarchy_api.rs` and equality support in `seed.rs`
- Build: Cargo release profile, already built before samples
- Timing: `REDA_PHASE_TIMING=1`

This machine is one measurement environment, not an architecture target. The
implementation remains CPU/OS/vendor independent and later runs the same
correctness result at thread counts 1, 2 and auto.

## Command

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture
```

The command was run three times serially. Cargo build time is excluded by the
test harness's reported execution time.

## Raw samples

All phase values are milliseconds.

| Sample | Child route | Child emit+verify | Child certify | Baseline route | Baseline emit+verify | Baseline certify | Moved route | Moved emit+verify | Moved certify | Reused route | Reused emit+verify | Reused certify | End to end |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 592 | 7 | 493 | 518 | 27 | 2819 | 596 | 25 | 2824 | absent | 26 | 2787 | 10.77 s |
| 2 | 570 | 6 | 456 | 497 | 26 | 2789 | 498 | 25 | 2768 | absent | 25 | 2844 | 10.56 s |
| 3 | 634 | 6 | 460 | 545 | 27 | 2915 | 500 | 25 | 2810 | absent | 32 | 2798 | 10.81 s |
| Median | 592 | 6 | 460 | 518 | 27 | 2819 | 500 | 25 | 2810 | absent | 26 | 2798 | 10.77 s |

## Evidence from the baseline

1. The changed-placement proposal pays a fresh routing phase; the unchanged
   proposal emits no placement or routing phase.
2. The unchanged proposal still pays a complete certification phase, as the
   correctness contract requires.
3. On this small fixture, the reused proposal's visible finishing work is
   `26 ms emit+verify + 2798 ms certify` at the median: certification dominates.
4. Current timing hides union and certification subphases. Layer 0 must expose
   them before attributing a later speedup.
5. Every sample passed the test that checks unchanged-plan pointer reuse,
   changed-placement replanning, rejected-proposal safety, an additional
   certifier call and identical baseline/reused candidate fingerprints.

## `ripple_adder8` budget-zero baseline

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='ripple_adder8'
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture
```

| Sample | Child route | Child emit+verify | Child certify | Top route | Top emit+verify | Top certify | End to end | Quality |
|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 561 ms | 6 ms | 411 ms | 16651 ms | 668 ms | 17071 ms | 35.5597063 s | 608 ticks / 70,603 blocks |
| 2 | 558 ms | 8 ms | 427 ms | 16826 ms | 669 ms | 16635 ms | 35.3174426 s | 608 ticks / 70,603 blocks |
| 3 | 549 ms | 6 ms | 421 ms | 16806 ms | 670 ms | 16759 ms | 35.4198535 s | 608 ticks / 70,603 blocks |
| Median | 558 ms | 6 ms | 421 ms | 16806 ms | 669 ms | 16759 ms | 35.4198535 s | 608 ticks / 70,603 blocks |

Top routing and top certification each consume about half of this case. The
foundation can remove duplicate certification work, but reaching the final 2x
goal also requires the later exact router and simulator layers; optimizing only
one side cannot meet the full objective.

## `multiplier4` budget-zero baseline

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='multiplier4'
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture
```

Three retained raw samples completed before implementation. The processes were
sampled through Windows' process counters while they ran; the largest retained
`PeakWorkingSet64` was 1,629,446,144 bytes (1.52 GiB). A first diagnostic run
whose console output was lost across a context boundary showed the same
execution shape but is not used as a timing sample.

| Sample | Leaf route | Leaf emit+verify | Leaf certify | Intermediate route | Intermediate emit+verify | Intermediate certify | Top route | Top emit+verify | Top certify | End to end | Quality |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 550 ms | 7 ms | 421 ms | 2,819 ms | 126 ms | 72,469 ms | 41,439 ms | 1,787 ms | 857,573 ms | 977.6524666 s | 1,039 ticks / 124,948 blocks |
| 2 | 559 ms | 6 ms | 418 ms | 2,820 ms | 126 ms | 72,895 ms | 41,658 ms | 1,836 ms | 878,644 ms | 999.3982700 s | 1,039 ticks / 124,948 blocks |
| 3 | 541 ms | 6 ms | 416 ms | 2,832 ms | 132 ms | 72,133 ms | 41,428 ms | 1,787 ms | 855,072 ms | 974.7866863 s | 1,039 ticks / 124,948 blocks |
| Median | 550 ms | 6 ms | 418 ms | 2,820 ms | 126 ms | 72,469 ms | 41,439 ms | 1,787 ms | 857,573 ms | 977.6524666 s | 1,039 ticks / 124,948 blocks |

The test passed with 337 gates and three compiled blocks. During the long top
certification interval the process accumulated CPU at approximately one core;
only the later manifest work used several cores. This is direct evidence for
parallelizing the existing 256-vector exhaustive truth stage, while retaining a
portable one-worker path and bounding concurrent world copies by the shared
worker budget.

## `alu8` budget-zero baseline

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='alu8'
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture
```

| Sample | Leaf route | Leaf emit+verify | Leaf certify | `alu4` route | `alu4` emit+verify | `alu4` certify | Top route | Top emit+verify | Top certify | End to end | Quality |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 2,676 ms | 35 ms | 8,648 ms | 13,606 ms | 716 ms | 9,240 ms | 32,504 ms | 4,154 ms | 53,726 ms | 125.9742277 s | 972 ticks / 213,833 blocks |
| 2 | 2,633 ms | 35 ms | 8,659 ms | 13,783 ms | 710 ms | 9,057 ms | 32,042 ms | 3,853 ms | 61,673 ms | 133.1050031 s | 972 ticks / 213,833 blocks |
| 3 | 2,623 ms | 36 ms | 8,724 ms | 13,604 ms | 715 ms | 10,088 ms | 34,918 ms | 4,383 ms | 58,876 ms | 134.6839150 s | 972 ticks / 213,833 blocks |
| Median | 2,633 ms | 35 ms | 8,659 ms | 13,606 ms | 715 ms | 9,240 ms | 32,504 ms | 4,154 ms | 58,876 ms | 133.1050031 s | 972 ticks / 213,833 blocks |

The test passed with 400 gates and three compiled blocks. Unlike
`multiplier4`, this three-level case is routing-heavy as well as certification-
heavy; it therefore prevents an exhaustive-only optimization from being
mistaken for complete corpus coverage.

## `ripple_adder8` Pull-X proposal baseline

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib block_pull_x_improves_ripple_adder8 -- --ignored --nocapture
```

| Sample | Child route | Child emit+verify | Child certify | Baseline route | Baseline emit+verify | Baseline certify | Pulled route | Pulled emit+verify | Pulled certify | End to end |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 542 ms | 6 ms | 426 ms | 16,805 ms | 657 ms | 17,005 ms | 16,518 ms | 664 ms | 16,461 ms | 69.46 s |
| 2 | 581 ms | 6 ms | 418 ms | 16,718 ms | 664 ms | 16,932 ms | 16,245 ms | 667 ms | 16,526 ms | 69.14 s |
| 3 | 556 ms | 6 ms | 412 ms | 16,686 ms | 667 ms | 16,392 ms | 16,368 ms | 717 ms | 19,353 ms | 71.54 s |
| Median | 556 ms | 6 ms | 418 ms | 16,718 ms | 664 ms | 16,932 ms | 16,368 ms | 667 ms | 16,526 ms | 69.46 s |

All three runs retained the same baseline quality
`(608, 70603, 1123332, 678)` and accepted the same pulled quality
`(608, 70601, 1123332, 680)`. The changed block placement paid a fresh routing
phase in every sample, so incumbent-plan reuse did not leak across the changed-
placement cache key.

## `multiplier4` Input Seam full-recompile baseline

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib input_seam_absorption_removes_the_child_refresh -- --ignored --nocapture
```

One retained raw sample completed in 1,867.20 seconds with a sampled
`PeakWorkingSet64` of 1,716,142,080 bytes (1.60 GiB).

| Candidate | Top route | Top emit+verify | Top certify | Compile wall time | Quality |
|---|---:|---:|---:|---:|---|
| Baseline | 41,093 ms | 1,869 ms | 849,729 ms | 893.0108973 s | `(1039, 124948, 3017412, 1070)` |
| Input seam | 45,647 ms | 1,818 ms | 850,495 ms | 898.3592750 s | `(1037, 124948, 3017412, 1068)` |

These rows are different circuit candidates: the seam deliberately improves
quality. Their per-candidate timing difference is not the before/after
implementation regression comparison, which compares the same candidate on
both revisions.

The shared lower-level compilation cost 553 ms routing, 6 ms emit/verify and
427 ms certification for the leaf, then 2,813 ms routing, 132 ms emit/verify
and 71,768 ms certification for the intermediate block.

This ignored fixture deliberately calls `compile_module_with_blocks` twice, so
both candidates re-plan and re-route. It is a valid full-recompile and single-
transaction baseline, but it does **not** measure incumbent-plan reuse. That
production-path behavior is covered by
`unchanged_block_placements_reuse_the_incumbent_plan`, where the reused proposal
has no placement/routing phase and still invokes the certifier.

## Prerequisite correctness baseline

All commands ran serially in the shared Windows target directory:

- `cargo test --lib compile::fragment_synth::hierarchy_api::tests -- --nocapture`
  — 17 passed, 0 failed, 4 ignored in 158.91 seconds.
- `cargo test --lib` — 910 passed, 0 failed, 71 ignored in 639.57 seconds.
- `cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --nocapture`
  — 1 passed, 0 failed in 418.17 seconds.
- `cargo clippy --lib --tests` — exit 0 with the branch's existing warning
  baseline.
- `git diff --check` — clean before these report additions.

This evidence is for the incumbent routed-parent reuse prerequisite. Every
production optimization below must rerun the affected focused tests, and the
final retained stack must rerun the complete matrix rather than inherit this
baseline result.

## Post-observability phase split

Same focused command as the top of this report, run after the Layer 0
observability change:

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture
```

The run passed 1 of 1 in 13.14 s. All values are milliseconds; `vectors` is
`WORK exhaustive_vectors` and `transitions` is `WORK manifest_transitions`.

| Candidate | Union | Structure +emit+verify | Timing | Equivalence | Exhaustive | Vectors | Manifest | Transitions | Metrics +fingerprints |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Leaf child | absent | 7 | 1 | 1 | 153 | 8 | 243 | 56 | 2 |
| Baseline parent | 3 | 29 | 4 | 3 | 2346 | 32 | 324 | 20 | 6 |
| Moved parent | 3 | 30 | 4 | 3 | 2357 | 32 | 333 | 20 | 6 |
| Reused parent, zero map | 3 | 30 | 4 | 3 | 2361 | 32 | 319 | 20 | 6 |
| Reused parent, explicit zero | 3 | 29 | 4 | 3 | 2451 | 32 | 314 | 20 | 7 |

What this replaces the pre-change picture with:

1. Certification is not one opaque block. On every parent, the exhaustive truth
   stage is roughly seven times the manifest sweep and about 85 % of the
   certification interval; union, timing analysis, equivalence and the metric
   fingerprints together stay under 20 ms.
2. The reused proposal still prints no placement and no routing phase, and
   still prints a complete certification label set. Incumbent-plan reuse and the
   full correctness contract are both visible in one trace.
3. The four parent candidates certify almost identically (2346–2451 ms
   exhaustive, 314–333 ms manifest) over the same 32 vectors and 20 transitions.
   That is the duplicate work Layer 1 targets, now measured rather than assumed.
4. The leaf's 56 transitions against 8 vectors invert the parent ratio, so an
   optimization tuned only to the parent shape would not be enough.

### Label completeness verification

The table above was sampled before the `PHASE compatibility+manifest_build`
label closed the last unattributed interval. The same focused command was rerun
after that label landed: passed 1 of 1 in 13.00 s,
`PHASE compatibility+manifest_build` appeared in every certification in pipeline
order, and the reused proposals still emitted no placement and no routing phase.
The label measures 0 ms on this fixture, so the timings above stand unchanged;
compatibility-view and manifest construction is real work but too small to
register here, and it will be visible on the large fixtures.
`cargo test --lib compile::fragment_synth::certification::tests -- --nocapture`
passed 10 of 10, 0 failed. Certification sub-phases now account for the whole
`PHASE certify` interval.

## Pending measurements

- Before/after single-certification-transaction samples.
- Post-change `multiplier4` and `alu8` samples.
- Thread counts 1, 2 and auto after portable parallel certification exists.
- Peak working set and final no-regression calculation.

## Retention table

| Change | Targeted phase before | Targeted phase after | Speedup | Largest regression | Decision |
|---|---:|---:|---:|---:|---|
| Complete phase observability | not measurable | measured, see the post-observability split | not a speedup change | none observed; focused test 1/1 in 13.00 s, certification tests 10/10 | RETAIN |
| One certification transaction | pending | pending | pending | pending | PENDING |
| Shared certification identity | pending | pending | pending | pending | PENDING |
| Immutable hierarchy context | pending | pending | pending | pending | PENDING |
