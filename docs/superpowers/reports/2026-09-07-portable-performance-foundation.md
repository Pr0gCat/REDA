# Portable Performance Foundation — Baseline and Retention Report

Status: portable speedup measured and verified. The retained stack now also
includes routing rollback journals, a static dust-topology cache and one
pristine simulator baseline per certification. Immutable hierarchy context
reuse was reverted by its measurement gate.

## Foundation measurement environment

- Date: 2026-09-07
- Worktree: `topology-aware-seed-v2-6f8f7e`
- Foundation HEAD: `95b6b9d` (`Revert "perf: reuse hierarchical compile invariants"`)
- Retained stack under measurement: incumbent routed-parent reuse, complete
  phase observability, one physical certification transaction and one shared
  certification identity
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
passed 10 of 10, 0 failed. Certification sub-phases account for the measured
certifier body inside `PHASE certify`; shape and ownership validation remain a
small unlabeled prefix.

## Revision-isolated focused measurements

The focused release fixture was run three times at each revision that bounds a
targeted change, in a detached worktree with the same target directory and no
concurrent Cargo process. Build time is excluded. These revisions all execute
four parent candidates, so the comparison is like-for-like.

| Revision | State | Raw end-to-end samples | Median |
|---|---|---|---:|
| `98003af` | before one-transaction change | 12.91 s, 12.85 s, 13.03 s | 12.91 s |
| `7ec3116` | after one transaction | 12.94 s, 12.90 s, 12.92 s | 12.92 s |
| `4b96b41` | after shared identity | 12.98 s, 12.90 s, 12.89 s | 12.90 s |
| `14aead2` | after immutable hierarchy context | 13.00 s, 12.99 s, 12.95 s | 12.99 s |
| `95b6b9d` | final stack after context revert | 13.15 s, 13.06 s, 13.14 s | 13.14 s |

The 10.77 s median near the beginning of this report is not comparable: that
older form of the fixture compiled three parent candidates, while every row
above compiles four. Its later 13.14 s observability sample and the
revision-isolated rows have the same candidate count.

### One physical certification transaction

The phase total is summed within each sample before taking the median; adding
component medians would incorrectly manufacture a 57 ms sample that never
occurred.

| Revision | Sample | Leaf outer + durable = total | Parent outer + durable = total |
|---|---:|---:|---:|
| `98003af` | 1 | 6 + 7 = 13 ms | 27 + 34 = 61 ms |
| `98003af` | 2 | 6 + 8 = 14 ms | 26 + 30 = 56 ms |
| `98003af` | 3 | 6 + 7 = 13 ms | 25 + 31 = 56 ms |
| `7ec3116` | 1 | 0 + 8 = 8 ms | 0 + 31 = 31 ms |
| `7ec3116` | 2 | 0 + 7 = 7 ms | 0 + 30 = 30 ms |
| `7ec3116` | 3 | 0 + 7 = 7 ms | 0 + 32 = 32 ms |

Before `7ec3116`, the parent median was therefore 56 ms. After the change it
pays only one durable transaction, with a 31 ms median:

```text
targeted speedup = 56 / 31 = 1.81x
timer-quantisation worst case = 56 / 31.999 = 1.75x
leaf cross-check = 13 / 7 = 1.86x
end-to-end change = (12.92 - 12.91) / 12.91 = +0.08%
```

The targeted phase clears 1.5x. End-to-end is dominated by the exhaustive and
manifest sweeps, so the saved tens of milliseconds remain below whole-test
noise.

### Shared certification identity

Again, each row is summed before the median is selected:

| Revision | Sample | Parent timing + equivalence + metrics = total |
|---|---:|---:|
| `7ec3116` | 1 | 5 + 4 + 6 = 15 ms |
| `7ec3116` | 2 | 4 + 3 + 6 = 13 ms |
| `7ec3116` | 3 | 4 + 3 + 6 = 13 ms |
| `4b96b41` | 1 | 0 + 0 + 3 = 3 ms |
| `4b96b41` | 2 | 0 + 0 + 3 = 3 ms |
| `4b96b41` | 3 | 0 + 0 + 3 = 3 ms |

Before `4b96b41`, a parent spent median `4 + 3 + 6 = 13 ms` in timing,
equivalence and metrics/fingerprints. After identity reuse the same displayed
phases are `0 + 0 + 3 = 3 ms`:

```text
targeted speedup = 13 / 3 = 4.33x
timer-quantisation worst case = 13 / (0.999 + 0.999 + 3.999) = 2.17x
metrics/fingerprints alone = 6 / 3 = 2.00x
end-to-end change = (12.90 - 12.92) / 12.92 = -0.15%
```

The public compatibility wrappers remain, but the identity-bearing certifier
entry is module-private and no third API layer is warranted.

### Immutable hierarchy context

`14aead2` removed repeated planning and flattening calls but added 96 net lines,
had no dedicated targeted-phase measurement, and changed the focused median
from 12.90 s to 12.99 s. The 0.70% difference is noise, not evidence of a
regression, but there is no 1.5x evidence with which to retain the change.

The hoist also depended on a context/variant compatibility invariant that no
production guard enforced. An independent Opus review therefore required the
change to be reverted before the representative corpus run. `95b6b9d` performs
that revert without rewriting history.

### Final retained raw phase samples

All values below are milliseconds except the final column. `absent` means that
incumbent-plan reuse emitted no routing phase.

| Sample | Candidate | Route | Union | Structure | Timing | Equiv. | Compat. | Exhaustive / vectors | Manifest / transitions | Metrics | Certify | End to end |
|---:|---|---:|---:|---:|---:|---:|---:|---|---|---:|---:|---:|
| 1 | leaf | 585 | absent | 6 | 0 | 0 | 0 | 154 / 8 | 262 / 56 | 1 | 427 | |
| 1 | baseline | 504 | 3 | 27 | 0 | 0 | 0 | 2369 / 32 | 345 / 20 | 3 | 2753 | |
| 1 | moved | 522 | 3 | 26 | 1 | 0 | 0 | 2367 / 32 | 324 / 20 | 3 | 2730 | |
| 1 | reused | absent | 3 | 26 | 0 | 0 | 0 | 2409 / 32 | 336 / 20 | 3 | 2783 | |
| 1 | reused zero | absent | 3 | 26 | 0 | 0 | 0 | 2426 / 32 | 318 / 20 | 3 | 2781 | 13.15 s |
| 2 | leaf | 550 | absent | 6 | 0 | 0 | 0 | 151 / 8 | 244 / 56 | 1 | 407 | |
| 2 | baseline | 499 | 3 | 26 | 0 | 0 | 0 | 2387 / 32 | 335 / 20 | 3 | 2761 | |
| 2 | moved | 502 | 3 | 26 | 0 | 0 | 0 | 2364 / 32 | 329 / 20 | 3 | 2731 | |
| 2 | reused | absent | 3 | 26 | 0 | 0 | 0 | 2392 / 32 | 349 / 20 | 3 | 2778 | |
| 2 | reused zero | absent | 3 | 26 | 0 | 0 | 0 | 2410 / 32 | 320 / 20 | 3 | 2769 | 13.06 s |
| 3 | leaf | 560 | absent | 6 | 0 | 0 | 0 | 152 / 8 | 243 / 56 | 1 | 406 | |
| 3 | baseline | 518 | 3 | 27 | 0 | 0 | 0 | 2433 / 32 | 329 / 20 | 3 | 2801 | |
| 3 | moved | 503 | 3 | 26 | 0 | 0 | 0 | 2381 / 32 | 334 / 20 | 3 | 2754 | |
| 3 | reused | absent | 3 | 26 | 0 | 0 | 0 | 2373 / 32 | 330 / 20 | 3 | 2741 | |
| 3 | reused zero | absent | 3 | 26 | 0 | 0 | 0 | 2411 / 32 | 341 / 20 | 3 | 2789 | 13.14 s |
| Median | leaf | 560 | absent | 6 | 0 | 0 | 0 | 152 / 8 | 244 / 56 | 1 | 407 | |
| Median | baseline | 504 | 3 | 27 | 0 | 0 | 0 | 2387 / 32 | 335 / 20 | 3 | 2761 | |
| Median | moved | 503 | 3 | 26 | 0 | 0 | 0 | 2367 / 32 | 329 / 20 | 3 | 2731 | |
| Median | reused | absent | 3 | 26 | 0 | 0 | 0 | 2392 / 32 | 336 / 20 | 3 | 2778 | |
| Median | reused zero | absent | 3 | 26 | 0 | 0 | 0 | 2411 / 32 | 320 / 20 | 3 | 2781 | 13.14 s |

The final 13.14 s median is 1.78% above the like-for-like 12.91 s pre-change
median, below the 5% no-regression gate.

## Cross-revision identity and quality

A temporary print-only test edit was run at `98003af`, `7ec3116`, `4b96b41`
and `95b6b9d`, then reversed before each clean disposable worktree was removed.
All four revisions produced exactly:

```text
Fingerprint("7e0ce2faad3ba9cc76806bf132c1b4ec09991be87f5ded77bd6439ba8540a75a")
QualityKey { observed_settle: 170, non_air_blocks: 7511,
  occupied_volume: 147108, static_routed_delay: ExactDelay(172) }
```

This is a cross-revision equality check, not merely the fixture's existing
within-run assertion.

The single transaction deliberately changes precedence for a candidate that is
invalid both structurally and during adaptation: structural certification now
reports first. The typed category mapping itself is unchanged, and the retained
fixtures cover adapter, emission and verification refusals.

## Representative retained corpus

Command:

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='ripple_adder8,multiplier4,alu8'
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture
```

Cargo's test summary was `1 passed, 0 failed` in 1134.64 s. The controller
shell merged stderr into stdout, so its wrapper reported status 1 despite the
explicit passing Cargo summary; this report does not claim a zero wrapper exit.

| Case | Retained | Baseline median | Change | Quality |
|---|---:|---:|---:|---|
| `ripple_adder8` | 35.4519594 s | 35.4198535 s | +0.09% | 608 ticks / 70,603 blocks |
| `multiplier4` | 975.3214186 s | 977.6524666 s | -0.24% | 1,039 ticks / 124,948 blocks |
| `alu8` | 123.8449690 s | 133.1050031 s | -6.96% | 972 ticks / 213,833 blocks |

No case exceeds the 5% regression limit, and all three quality pairs match
their baseline exactly. The retained corpus peak working set was 1,640,177,664
bytes versus 1,629,446,144 bytes at baseline, a 0.66% increase.
The corpus harness exposes ticks and block count for this comparison; complete
fingerprint, occupied-volume and static-delay equality was checked by the
focused fixture above, not by these three corpus rows.

The post-change phase split confirms the next portable target:

- `ripple_adder8` top: 16,929 ms routing and 17,328 ms certification.
- `multiplier4` intermediate: 2,568 ms routing and 71,625 ms certification;
  top: 41,923 ms routing and 857,916 ms certification. Its exhaustive portions
  alone were 69,597 ms and 832,622 ms for 256 vectors each.
- `alu8` leaf: 2,214 ms routing and 8,890 ms certification; `alu4`: 13,864 ms
  routing and 9,623 ms certification; top: 34,796 ms routing and 53,744 ms
  certification.

`multiplier4` directly supports deterministic multi-core exhaustive work as
the next layer, while `alu8` preserves the routing-heavy counterexample needed
to prevent over-specialising for that one case.

The Input Seam fixture was not rerun. This is a deliberate deviation from Task
6's literal three-run instruction: the fixture takes about 31 minutes, compiles
two different candidates from scratch and cannot isolate incumbent context
reuse or either retained change. The independent Opus retention adjudication
accepted the revision-isolated focused samples as the valid retention comparison;
this report makes no retention claim from the Input Seam result.

## Complete retained-stack verification

All commands ran serially at `95b6b9d`:

- `cargo test --lib` — exit 0; 911 passed, 0 failed, 71 ignored in 668.42 s.
- pinned seven-segment contract — exit 0; 1 passed, 0 failed in 421.08 s.
- `cargo clippy --lib --tests` — exit 0 with the branch's existing warnings.
- `git diff --check` — clean.
- Tracked worktree — clean before this report edit.

## Retention table

| Change | Targeted phase before | Targeted phase after | Speedup | Largest observed regression | Decision |
|---|---:|---:|---:|---:|---|
| Complete phase observability | not measurable | measured | diagnostic | none | RETAIN |
| One certification transaction | 56 ms | 31 ms | 1.81x; 1.75x quantised worst case | final focused stack +1.78%; corpus +0.09% | KEEP |
| Shared certification identity | 13 ms | 3 ms | 4.33x; 2.17x quantised worst case | final focused stack +1.78%; corpus +0.09% | KEEP |
| Immutable hierarchy context | unmeasured | unmeasured | no 1.5x evidence | no reliable regression claim | REVERTED in `95b6b9d` |

The retained changes meet their targeted-phase gates, preserve exact candidate
identity and quality, and keep every representative regression under 5%. They
do not materially improve whole-compile wall time because exhaustive simulation
and routing dominate; they are foundation cleanup, not the final 2x result.

## Router ancestry milestone — 2026-09-08

The fine-grained ancestry profiler was removed because its per-lookup timing
introduced a material observer effect. Clean `route_nets` medians were used
for the retention decision instead.

| Circuit | Clean baseline | Candidate | Speedup | Quality |
|---|---:|---:|---:|---|
| ripple_adder8 | 17.892804 s | 11.246720 s | 1.59x | unchanged: 608 ticks / 70,603 blocks |
| alu8 | 50.313820 s | 30.953307 s | 1.63x | unchanged: 972 ticks / 213,833 blocks |

The candidate reuses one predecessor-chain snapshot per expansion and performs
cheap candidate rejection early only where coordinate arithmetic is safe.
Boundary coordinates retain the original predicate order.

Verification evidence: all 17 routing tests and the pinned seven-segment IO
contract passed after the boundary fallback. The full library suite passed
930/930 with 74 ignored, and `cargo clippy --lib` exited 0 with the branch's
existing warnings. Independent static review: APPROVED.

## Settled-source certification milestone — 2026-09-08

Manifest certification now settles each contiguous source vector once, clones
that complete simulator state for its destinations, and preserves the original
manifest order. Source groups are packed into contiguous, transition-weighted
worker batches. The final destination consumes the prepared simulator, so the
change performs exactly one complete world clone per transition rather than one
extra clone per source group. Unchanged pinned and lever inputs also avoid
creating dirty entries, and the unused certification observer was removed.

Release measurements used the unchanged hierarchical floorplan acceptance test,
three runs per circuit, with manifest time summed across hierarchy levels:

| Circuit | Baseline samples | Retained samples | Median speedup | Quality |
|---|---:|---:|---:|---|
| `ripple_adder8` | 19.613 / 19.975 / 19.628 s | 10.038 / 10.102 / 10.310 s | 1.94x | unchanged: 608 ticks / 70,603 blocks |
| `alu8` | 75.313 / 81.598 / 73.250 s | 38.106 / 38.532 / 37.984 s | 1.98x | unchanged: 972 ticks / 213,833 blocks |

The leaf exhaustive medians moved from 0.160 to 0.154 seconds for
`ripple_adder8`, and from 1.949 to 1.704 seconds for `alu8`. Those are secondary
results; the retained gate is the manifest phase.

Two alternatives were measured and rejected. Removing the dead observer and
unchanged writes alone reached only 0.93x on `ripple_adder8` and 1.05x on
`alu8`. Splitting transitions evenly before source grouping improved theoretical
core occupancy but repeated expensive source settling at chunk boundaries; its
`ripple_adder8` median was 12.221 seconds, 20.98% slower than the retained
weighted whole-group result.

Correctness is pinned by a fresh-per-transition reference sweep, fixed legacy
measurement and cap fields, logical-order error reduction, and complete
candidate/certificate/manifest/metric equality at one, two and four workers.
The existing automatic memory policy remains a conservative heuristic based on
world volume and four-copy headroom; this milestone does not claim measured peak
RSS or a hard cross-machine memory ceiling. No GPU path or new dependency was
added.

## Routing rollback and simulator topology-cache milestone — 2026-09-09

Retained implementation commit: `f6e54ea` (`perf: cache dust topology and
journal routing attempts`), measured against parent `75a2cd8`.

The router now journals only cells touched by one failed route attempt and rolls
them back on drop. The shipping guarded router uses that transaction directly;
the trait fallback still clones and calls `route_owned`, preserving compatibility
with external router implementations. Certification builds one pristine
simulator and clones it for independent vectors and source groups. The simulator
caches static dust weak-components and directed adjacency behind a clone-aware
World lineage plus topology epoch; dynamic power and lit changes do not rebuild
the cache.

Exact budget-zero benchmark command, run serially with a prebuilt release
profile and no certification environment overrides:

```powershell
$test = 'compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan'
$env:REDA_PHASE_TIMING = '1'
Remove-Item Env:REDA_CERT_THREADS -ErrorAction SilentlyContinue
Remove-Item Env:REDA_CERT_MEMORY_BYTES -ErrorAction SilentlyContinue
$env:REDA_EXTRA_CIRCUITS = '<one circuit name>'
cargo test --release --lib $test -- --exact --ignored --nocapture --test-threads=1
```

End-to-end time is the harness's `CIRCUIT ... in ...` interval, excluding Cargo
build time. The hard gate was a 1.5x median speedup on `ripple_adder8` and
`alu8`; `multiplier4` is the independent dust-heavy larger case.

| Circuit | Baseline median | Retained samples | Retained median | Speedup | Quality |
|---|---:|---:|---:|---:|---|
| `ripple_adder8` | 21.7669945 s | 13.0702246 / 13.1204520 / 13.2202067 s | 13.1204520 s | 1.66x | unchanged: 608 ticks / 70,603 blocks |
| `alu8` | 75.0356823 s | 46.7984661 / 46.7875583 / 46.5762006 s | 46.7875583 s | 1.60x | unchanged: 972 ticks / 213,833 blocks |
| `multiplier4` | 230.5988662 s | 162.2779329 / 161.8450070 / 164.5257223 s | 162.2779329 s | 1.42x | unchanged: 1,039 ticks / 124,948 blocks |

The routing phase moved from about 29.7 s to 12.8–12.9 s on `alu8`, and from
about 31.9 s to a three-sample median of 11.366 s on `multiplier4`. The retained
`ripple_adder8` routing samples sum to 4.850 / 4.809 / 4.857 s across hierarchy
levels. On the final `multiplier4` samples, top exhaustive certification remains
the dominant 126–129 s phase, so the next speed project has a measured target
rather than another routing rewrite.

The fixed worker-count acceptance matrix was changed from `1/2/auto` to
`1/2/4`. It compares case and candidate fingerprints, all metrics, emitted-world
fingerprint, IO positions, observations, stop reason and the non-empty proposal
trace:

| Circuit | 1 worker | 2 workers | 4 workers | Exact result |
|---|---:|---:|---:|---|
| `ripple_adder8` | 84.147 s | 73.793 s | 45.014 s | PASS; 590 ticks / 70,659 blocks / trace 1 |
| `alu8` | 288.472 s | 238.094 s | 143.733 s | PASS; 972 ticks / 213,833 blocks / trace 1 |

Final verification:

- `cargo test --lib -- --test-threads=1`: 941 passed, 0 failed, 74 ignored in
  1035.57 s. The later Ponytail cleanup changed tests only; affected focused
  routing, cache and certification tests were rerun and passed.
- Fixed `1/2/4` release matrix over `ripple_adder8,alu8`: passed in 873.70 s.
- Pinned seven-segment release contract: 1 passed in 12.22 s; all eleven fixed
  coordinates, handover states and flat/hierarchical fingerprints agree.
- `cargo clippy --lib --tests`: exit 0 with the branch's existing warning set.
- `git diff --check`: clean.
- Independent routing, simulator/certification and Ponytail reviews: APPROVED;
  no Critical or Important findings.
- Claude Sonnet's read-only report and arithmetic audit: APPROVED.

Two attempted extensions were rejected. Caching palette state transitions had
no measurable benefit and was removed. Splitting manifest transitions without
respecting source groups repeated source settling and remained 20.98% slower
than the retained grouping. No fixture-specific branch, GPU path or dependency
was added.
