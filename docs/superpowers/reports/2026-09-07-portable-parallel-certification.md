# Portable Parallel Certification — Benchmark and Retention Report (draft)

Status: **DRAFT — measurements pending.** This file records the exact commands,
already-verified facts and the layout the final report must fill in. Every
value below is either cited from a prior commit/report or marked
`TBD_MEASURED`. No Cargo command has been run to produce this draft.

## Scope and gates (two distinct thresholds — do not conflate)

1. **This layer's PHASE gate (retention gate for Task 5):** at least **1.5x**
   speedup on the `PHASE exhaustive` interval of budget-zero `multiplier4`,
   identical outputs (fingerprints, metrics, traces, cap counters, quality) at
   thread counts 1, 2 and auto, no representative case more than 5% slower at
   the same thread setting, and bounded peak memory tracking
   `workers * per_worker_bytes` within `WORLD_COPY_HEADROOM`.
   Source: `docs/superpowers/plans/2026-09-07-portable-parallel-certification.md`
   (plan header, "Retention gate") and Task 5 body.
2. **Final all-layer gate (spec-level, not decided by this report):** at least
   **2.0x geometric-mean end-to-end** speedup on the full representative
   corpus, no representative case more than 5% slower, each retained milestone
   individually clearing its own 1.5x targeted-phase gate.
   Source: `docs/superpowers/specs/2026-09-07-portable-extreme-generation-performance.md:47-49,297-299`.

This report decides only gate 1 (KEEP/REVERT this layer). It does not and
cannot certify gate 2; that requires the same release matrix on the full
representative corpus across all retained layers plus a second distinct
machine, per the spec.

## Environment

- Date: 2026-09-08 (report authored); measurement dates: `TBD_MEASURED`.
- Worktree: `topology-aware-seed-v2-6f8f7e`.
- Host logical processors: 12.
- Host physical memory: 68,664,922,112 bytes.
- Build: Cargo release profile, built before samples (build time excluded from
  all reported wall times).
- Timing: `REDA_PHASE_TIMING=1`.
- Retained foundation revision (comparison baseline): `95b6b9d` (see
  `docs/superpowers/reports/2026-09-07-portable-performance-foundation.md`).
- Layer commits under test: `f899d6c` (Task 1, ordered chunk helper),
  `a6588b9` (Task 2, `perf: parallelize exhaustive certification`),
  `685dfa5` (Task 3, `perf: coordinate compile-wide worker budgets`),
  `611349a`..`1e62683` (Task 4, thread-count matrix tests: `test: cover
  certification thread-count matrix`, `test: harden certification thread
  matrix`, `test: reject empty certification matrix`).
- Temporary instrumentation present in the working tree at draft time
  (`src/compile/fragment_synth/certification.rs`, `src/compile/fragment_synth/seed.rs`):
  a `REDA_PHASE_TIMING`-gated `WORK sweep_budget` diagnostic line reporting
  `world`, `volume`, `per_worker_bytes`, `memory_budget_bytes`, `ceiling`,
  `requested`, `explicit` and `granted`, plus fingerprint capture in `seed.rs`.
  **This instrumentation is temporary and must be reverted before the final
  report/commit; it exists only to source the `per_worker_bytes` /
  memory-ceiling rows below.**

## Foundation values reused as-is (not re-measured)

At `95b6b9d`, only the exhaustive stage was serial; the manifest sweep already
honored `REDA_CERT_THREADS` and ran parallel at that revision (Task 2 of this
layer parallelizes exhaustive, not manifest). The foundation report's
budget-zero rows below were therefore captured with `REDA_CERT_THREADS` unset
(auto), not under a generic "serial" setting, and are not a same-setting
comparison point for exhaustive until paired with this layer's own auto rows.
Because the exhaustive code path is otherwise identical serial code at `95b6b9d`
regardless of `REDA_CERT_THREADS`, its `PHASE exhaustive` denominator is
thread-invariant and the foundation's auto-setting exhaustive numbers double as
the thread=1 exhaustive comparison point. End-to-end is not thread-invariant at
`95b6b9d` (manifest parallelism still affects it), so the required same-setting
end-to-end comparison needs fresh thread=1 *and* thread=2 rows from this layer
below, not a foundation citation, at both settings:

| Case | Top/only `PHASE exhaustive` median | End-to-end median | Quality |
|---|---:|---:|---|
| `ripple_adder8` (top) | not isolated from `PHASE certify` in that report's table; top certify 17,071 ms (sample 1)/16,635/16,759 ms, `PHASE exhaustive` not separately itemized for this case | 35.4198535 s | 608 ticks / 70,603 blocks |
| `multiplier4` (top) | top exhaustive alone: 832,622 ms (from "post-change phase split", corpus run) | 977.6524666 s | 1,039 ticks / 124,948 blocks |
| `alu8` (top) | not itemized to exhaustive alone in that report | 133.1050031 s | 972 ticks / 213,833 blocks |

These are auto-setting rows from `95b6b9d`, required by Task 5 step 3 as the
"same retained foundation revision" comparison point. Because `95b6b9d`'s
exhaustive code path is identical regardless of `REDA_CERT_THREADS` (only
manifest was parallel there), these auto-setting exhaustive values double as
the thread=1 exhaustive baseline; the new thread=1 `PHASE exhaustive` samples
taken in this layer (below) must reproduce them within noise before any
thread=2/auto exhaustive speedup is trusted. End-to-end has no such shortcut:
`95b6b9d` end-to-end already reflects parallel manifest at auto, so this
layer's own fresh thread=1 *and* thread=2 end-to-end rows (below) are the
same-setting comparison, not this foundation table.

## Raw baseline `95b6b9d` samples (cited, not this layer's own)

The old baseline (`95b6b9d`) report/progress log has no worker-count or
memory diagnostics (no `WORK exhaustive_workers`, no `per_worker_bytes`, no
memory ceiling instrumentation existed at that revision), so for every row
below, actual exhaustive workers = 1 by source (the exhaustive stage was
serial code at `95b6b9d`, per the "Foundation values reused as-is" section
above) and the per-worker memory model is n/a at that revision.

| Case | Threads | Sample | Intermediate exhaustive | Top exhaustive | Manifest (intermediate/top) | End-to-end | Wrapper wall time | Peak working set | Fingerprint (case) | Quality |
|---|---|---:|---:|---:|---|---:|---:|---:|---|---|
| `multiplier4` | 1 | 1 | 72,553 ms | 853,766 ms | 9,602/114,633 ms | 1100.7550576 s | 1100.9846 s | 1,622,892,544 B | `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048` | 1039 ticks / 124,948 blocks / occupied volume 3,017,412 / static delay 1070 |
| `multiplier4` | 1 | 2 | 73,379 ms | 849,956 ms | 9,648/113,578 ms | 1096.8888845 s | 1096.9549 s | 1,621,102,592 B | `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048` | 1039 ticks / 124,948 blocks / occupied volume 3,017,412 / static delay 1070 |
| `multiplier4` | 1 | 3 | 73,764 ms | 850,937 ms | 9,656/113,715 ms | 1098.4474287 s | 1098.4598 s | 1,624,129,536 B | `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048` | 1039 ticks / 124,948 blocks / occupied volume 3,017,412 / static delay 1070 |
| `multiplier4` | 1 | Median | 73,379 ms | 850,937 ms | 9,648/113,715 ms | 1098.4474287 s | 1098.4598 s | 1,622,892,544 B (max across samples 1,624,129,536 B) | `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048` | 1039 ticks / 124,948 blocks / occupied volume 3,017,412 / static delay 1070 |
| `multiplier4` | 2 | 1 | 73,297 ms (leaf 162 ms) | 848,035 ms (sum leaf+intermediate+top 921,494 ms) | 5,145/60,093 ms (leaf 770 ms; sum leaf+intermediate+top manifest 66,008 ms) | 1036.3501146 s | 1036.4190 s | 1,621,880,832 B | `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048` (matches thread=1 exactly) | 1039 ticks / 124,948 blocks / occupied volume 3,017,412 / static delay 1070 (matches thread=1 exactly) |
| `multiplier4` | auto | 1 (cited) | n/a (not itemized in foundation report) | 832,622 ms (foundation, "post-change phase split", corpus run) | n/a (not itemized in foundation report) | 977.6524666 s (foundation) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | 1,039 ticks / 124,948 blocks (foundation) |
| `ripple_adder8` | 1 | 1 (guard) | n/a (no intermediate level for this case) | 0 ms (leaf 164 ms) | n/a/81,366 ms (leaf manifest 1,323 ms) | 101.8152310 s | 102.0559 s | 680,316,928 B | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` | 608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678 |
| `ripple_adder8` | 2 | 1 (guard) | n/a (no intermediate level for this case) | 0 ms (leaf 159 ms) | n/a/44,194 ms (leaf manifest 781 ms) | 64.0509431 s | 64.0753 s | 680,632,320 B | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` (matches thread=1 exactly) | 608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678 (matches thread=1 exactly) |
| `ripple_adder8` | auto | 1 (cited) | n/a (not itemized in foundation report) | not isolated from `PHASE certify` in foundation report; top certify 17,071 ms (sample 1)/16,635/16,759 ms | n/a (not itemized in foundation report) | 35.4198535 s (foundation) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | 608 ticks / 70,603 blocks (foundation) |
| `alu8` | 1 | 1 (guard) | 0 ms (alu4 level; leaf 8,571 ms) | 0 ms | 42,442/239,383 ms (leaf manifest 1,963 ms) | 349.0261563 s | 349.1496 s | 1,633,325,056 B | `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02` | 972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204 |
| `alu8` | 2 | 1 (guard) | 0 ms (alu4 level; leaf 8,482 ms) | 0 ms | 22,654/127,955 ms (leaf manifest 1,061 ms) | 216.7815886 s | 216.8377 s | 1,639,493,632 B | `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02` (matches thread=1 exactly) | 972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204 (matches thread=1 exactly) |
| `alu8` | auto | 1 (cited) | n/a (not itemized in foundation report) | n/a (not itemized to exhaustive alone in foundation report) | n/a (not itemized in foundation report) | 133.1050031 s (foundation) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | n/a (not itemized in foundation report) | 972 ticks / 213,833 blocks (foundation) |

`multiplier4` thread=1 summed all-level (leaf+intermediate+top) exhaustive
median: 924,859 ms. `multiplier4` thread=1 summed all-level manifest median:
124,682 ms. Both summed medians are additional context alongside the
intermediate/top-only rows above, not a replacement for them.

`multiplier4` thread=2 has only a single baseline sample so far (no median
across three samples). Its summed all-level exhaustive is 921,494 ms and its
summed all-level manifest is 66,008 ms. Whether the same-setting end-to-end
margin against this layer's current thread=2 numbers requires two more
baseline thread=2 samples (per the ledger's "escalate only if the observed
margin against the 1.5x gate is under 20%" rule) is not yet decided and will
be determined after this layer's current thread=2 measurement is in hand.

`auto` rows marked "(cited)" above reuse the foundation report's auto-setting
values as-is (see "Foundation values reused as-is" table above for
`multiplier4`/`ripple_adder8`/`alu8` end-to-end and quality); per-sample
intermediate/top exhaustive, manifest split, wrapper wall time, peak working
set and fingerprint were not itemized at that granularity in the foundation
report/progress log and are marked `n/a` rather than invented. `1` and `2`
thread-setting rows for `ripple_adder8`/`alu8` are now filled in above from
the ledger's single guard sample per setting (per plan Task 5 sampling: one
sample each, not three); no `95b6b9d` median exists for these guard cases
because only one sample was taken at each setting.

## Sampling plan (per plan Task 5 + Opus binding adjudication)

- **`multiplier4` (primary, budget-zero):** 3 samples each at
  `REDA_CERT_THREADS=1`, `2`, and auto (variable unset). This is the layer's
  primary PHASE-gate fixture.
- **`ripple_adder8` and `alu8` (balanced/routing-heavy guards):** 1 sample
  each per thread setting (1, 2, auto) — not 3.
- **Baseline thread=1 re-verification:** needs 3 `multiplier4` samples (same
  as above) plus the guard singles, since thread=1 must reproduce the
  retained-foundation serial numbers before any speedup claim is trusted.
- **Baseline thread=2 re-verification:** 1 `multiplier4` sample plus guard
  singles; escalate to additional samples only if the observed margin against
  the 1.5x gate is under 20%.
- The exhaustive stage's `PHASE exhaustive` denominator is thread-invariant by
  code identity at the caller-thread (thread=1) path — it is the same serial
  loop at `95b6b9d` and at this layer's thread=1 setting — reinforced by the
  plan's stop conditions and Task 2's parallel/serial equivalence tests
  (`certification::tests::parallel_manifest_sweep_matches_serial_results`,
  `certification::tests::exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count`).
  End-to-end is not thread-invariant (manifest and scheduling overhead still
  vary), so end-to-end speedup must come from this layer's own same-setting
  t1/t2/auto rows, never from citing the foundation report.

## Measurement method

**Test-name collision discovered and fixed.** The short substring
`every_hierarchical_circuit`, used in an earlier draft of this method and
still present in this test's own `#[ignore]` message
(`src/compile/fragment_synth/seed.rs:4889`), is not unique: it also matches
`compile::fragment_synth::seed::tests::every_hierarchical_circuit_agrees_across_certification_thread_counts`,
Task 4's release-only, serial thread-count-matrix test
(`src/compile/fragment_synth/seed.rs:5187`). libtest's substring filter
selects both tests whenever that short form is used, and libtest's default
test harness runs matched tests concurrently unless `--test-threads=1` is
passed. An earlier sample taken with the short substring and no
`--test-threads=1` therefore launched Task 5's budget-zero exhaustive test
*and* Task 4's serial thread-count-matrix test as two concurrent compiles in
the same process, invalidating both that sample's timing (each competed with
the other for CPU) and its peak-memory reading (3,231,039,488 B, reflecting
two overlapping certification runs' working sets rather than one); that
sample is excluded and was not carried into any table above.

At the retained baseline revision (`95b6b9d`), only one test matched the
short substring — Task 4's thread-count-matrix tests did not exist yet at
that revision — so the collision could not have corrupted any baseline row
above; it is specific to this layer's own commits (`611349a`..`1e62683`) and
only affects freshly taken samples of this layer, not the cited `95b6b9d`
values. Even so, this method now uses the exact test path uniformly, for both
this layer's samples and any future re-verification, rather than relying on
revision-specific substring uniqueness.

Each revision under test was prebuilt once with `cargo test --release --lib
compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan
--no-run`, producing a fixed `target/release/deps/reda-<hash>.exe` for that
revision. Every timed sample then invoked that direct test binary
(`target/release/deps/reda-<hash>.exe
compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan
--exact --ignored --nocapture --test-threads=1`) with the required env vars
(`REDA_PHASE_TIMING`, `REDA_EXTRA_CIRCUITS`, `REDA_CERT_THREADS`) directly,
not through `cargo test`, so no Cargo/rustc invocation is included in any
timed sample; the direct binary is the same lib-test executable Cargo would
otherwise launch on `cargo test --release --lib
compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan
-- --exact --ignored --nocapture --test-threads=1`, so results are equivalent
to running it through Cargo minus the Cargo dispatch/build-check overhead.
`--exact` pins the match to this single test (rejecting the Task 4 collision
above), and `--test-threads=1` guarantees no other matched test can run
concurrently with it even if the exact-match guarantee were ever weakened. A
PowerShell parent process launches the direct binary, polls its
`PeakWorkingSet64` every 250 ms for the process lifetime, and retains
`max(sampled peak, final peak)` as the reported peak working set; the poll
count for each sample is recorded alongside the timing row so a too-short run
(few polls) is visible rather than silently trusted. The parent redirects and
preserves the child's stdout/stderr so the `PHASE` lines and fingerprints
remain available for the row. Any rustc process observed running during a
sample's window (e.g. a concurrent build) voids that sample and it must be
retaken. The test binary's own internal `in ...s`-style phase timing is the
compiler's end-to-end measurement of that phase and is the primary reported
figure; the wrapper's total elapsed time around the child process is recorded
alongside it only as an external cross-check, not a replacement.

## Raw samples — `multiplier4` (budget-zero, primary fixture)

Command (run three times per thread setting, serially):

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='multiplier4'
$env:REDA_CERT_THREADS='1'   # then '2', then Remove-Item Env:REDA_CERT_THREADS for auto
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan -- --exact --ignored --nocapture --test-threads=1
```

| Threads | Sample | `PHASE exhaustive` (top) | `WORK exhaustive_workers` | End-to-end | Peak working set | `per_worker_bytes` | Memory ceiling | Fingerprint | Quality |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 1 | 1 | 821,498 ms (leaf 154 ms; intermediate 69,985 ms; sum 891,637 ms) | requested=1 explicit=Some(1) granted=1 | 1060.9667193 s (wrapper 1061.0289 s) | 1,625,018,368 B (peak private 1,666,367,488 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 1 | 2 | 821,137 ms (leaf 158 ms; intermediate 70,662 ms; sum 891,957 ms) | requested=1 explicit=Some(1) granted=1 | 1061.0744303 s (wrapper 1061.2488 s) | 1,624,608,768 B (peak private 1,665,822,720 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 1 | 3 | 825,022 ms (leaf 151 ms; intermediate 70,405 ms; sum 895,578 ms) | requested=1 explicit=Some(1) granted=1 | 1065.7085087 s (wrapper 1065.9130 s) | 1,621,835,776 B (peak private 1,663,758,336 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 1 | Median | 821,137 ms (leaf 158 ms; intermediate 70,662 ms; sum 891,957 ms) | requested=1 explicit=Some(1) granted=1 | 1061.0744303 s (wrapper 1061.2488 s) | 1,624,608,768 B (max 1,625,018,368 B); peak private median 1,665,822,720 B | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 2 | 1 | 447,895 ms (leaf 154 ms; intermediate 38,302 ms; sum 486,351 ms) | leaf granted=1; intermediate/top granted=2 | 598.4989074 s (wrapper 598.6341 s) | 1,624,854,528 B (peak private 1,666,154,496 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 2 | 2 | 441,268 ms (leaf 153 ms; intermediate 38,182 ms; sum 479,603 ms) | leaf granted=1; intermediate/top granted=2 | 594.1709836 s (wrapper 594.2089 s) | 1,622,532,096 B (peak private 1,665,589,248 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 2 | 3 | 438,419 ms (leaf 154 ms; intermediate 38,242 ms; sum 476,815 ms) | leaf granted=1; intermediate/top granted=2 | 588.8557248 s (wrapper 589.0337 s) | 1,625,137,152 B (peak private 1,668,276,224 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| 2 | Median | 441,268 ms (leaf 153 ms; intermediate 38,182 ms; sum 479,603 ms) | leaf granted=1; intermediate/top granted=2 | 594.1709836 s (wrapper 594.2089 s) | 1,624,854,528 B (max 1,625,137,152 B); peak private median 1,666,154,496 B | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| auto | 1 | 174,128 ms (leaf 155 ms; intermediate 15,334 ms; sum 189,617 ms) | requested=12; leaf exhaustive/manifest granted=1/7; intermediate granted=12/4; top granted=11/4 | 279.5675934 s (wrapper 279.7165 s) | 1,636,679,680 B (peak private 1,678,127,104 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| auto | 2 | 174,208 ms (leaf 154 ms; intermediate 14,950 ms; sum 189,312 ms) | requested=12; leaf exhaustive/manifest granted=1/7; intermediate granted=12/4; top granted=11/4 | 279.1949518 s (wrapper 279.4364 s) | 1,627,254,784 B (peak private 1,669,943,296 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| auto | 3 | 174,541 ms (leaf 156 ms; intermediate 15,333 ms; sum 190,030 ms) | requested=12; leaf exhaustive/manifest granted=1/7; intermediate granted=12/4; top granted=11/4 | 280.2311574 s (wrapper 280.4929 s) | 1,624,862,720 B (peak private 1,670,864,896 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |
| auto | Median | 174,128 ms (leaf 155 ms; intermediate 15,334 ms; sum 189,617 ms) | requested=12; leaf exhaustive/manifest granted=1/7; intermediate granted=12/4; top granted=11/4 | 279.5675934 s (wrapper 279.7165 s) | 1,627,254,784 B (max 1,636,679,680 B); peak private median 1,670,864,896 B (max 1,678,127,104 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |

Current `1e62683` thread=1 sample 1 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 154/69,985/821,498 ms (sum 891,637 ms);
manifest 1,264/9,326/110,424 ms (sum 121,014 ms); end-to-end 1060.9667193 s
(wrapper 1061.0289 s); peak working set 1,625,018,368 B, peak private
1,666,367,488 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline; every sweep printed
requested=1 explicit=Some(1) granted=1. Top world 3771x6x267, volume
6,041,142, per_worker_bytes 96,658,272, budget 1,073,741,824 B, 1-GiB auto
ceiling 11.

Current `1e62683` thread=1 sample 2 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 158/70,662/821,137 ms (sum 891,957 ms);
manifest 1,263/9,381/109,742 ms (sum 120,386 ms); end-to-end 1061.0744303 s
(wrapper 1061.2488 s); peak working set 1,624,608,768 B, peak private
1,665,822,720 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline; every sweep printed
requested=1 explicit=Some(1) granted=1. per_worker_bytes 96,658,272, budget
1,073,741,824 B, 1-GiB auto ceiling 11.

Current `1e62683` thread=1 sample 3 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 151/70,405/825,022 ms (sum 895,578 ms);
manifest 1,263/9,381/111,091 ms (sum 121,735 ms); end-to-end 1065.7085087 s
(wrapper 1065.9130 s); peak working set 1,621,835,776 B, peak private
1,663,758,336 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline; every sweep printed
requested=1 explicit=Some(1) granted=1. per_worker_bytes 96,658,272, budget
1,073,741,824 B, 1-GiB auto ceiling 11.

Current `1e62683` multiplier4 thread=1 median (samples 1-3): end-to-end
1061.0744303 s (wrapper 1061.2488 s), 3.40% faster than the retained baseline
thread=1 median (1098.4474287 s); summed all-level exhaustive 891,957 ms,
3.56% faster than the retained baseline (924,859 ms); summed all-level
manifest 121,014 ms. Peak working set median 1,624,608,768 B (max
1,625,018,368 B), peak private median 1,665,822,720 B. Identity is exact in
all three samples and every sweep granted exactly one worker. This median
reproduces the retained baseline's serial exhaustive numbers within noise, as
required before any thread=2/auto exhaustive speedup claim is trusted.

Current `1e62683` thread=2 sample 1 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 154/38,302/447,895 ms (sum 486,351 ms);
manifest 690/5,000/58,133 ms (sum 63,823 ms); end-to-end 598.4989074 s
(wrapper 598.6341 s); peak working set 1,624,854,528 B, peak private
1,666,154,496 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline (exact match). Sweeps printed
granted=1 for the leaf phase and granted=2 for the intermediate/top phases
(actual worker counts differ by phase at `REDA_CERT_THREADS=2`); per_worker_bytes
96,658,272, budget 1,073,741,824 B, 1-GiB auto ceiling 11.

Current `1e62683` thread=2 sample 2 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 153/38,182/441,268 ms (sum 479,603 ms);
manifest 745/6,458/58,210 ms (sum 65,413 ms); end-to-end 594.1709836 s
(wrapper 594.2089 s); peak working set 1,622,532,096 B, peak private
1,665,589,248 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline (exact match). Sweeps printed
granted=1 for the leaf phase and granted=2 for the intermediate/top phases
(actual worker counts differ by phase at `REDA_CERT_THREADS=2`); per_worker_bytes
96,658,272, budget 1,073,741,824 B, 1-GiB auto ceiling 11.

Current `1e62683` thread=2 sample 3 (exact single test), raw all-level detail:
leaf/intermediate/top exhaustive 154/38,242/438,419 ms (sum 476,815 ms);
manifest 708/5,037/57,858 ms (sum 63,603 ms); end-to-end 588.8557248 s
(wrapper 589.0337 s); peak working set 1,625,137,152 B, peak private
1,668,276,224 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline (exact match). Sweeps printed
granted=1 for the leaf phase and granted=2 for the intermediate/top phases;
per_worker_bytes 96,658,272, budget 1,073,741,824 B, 1-GiB auto ceiling 11.

Current `1e62683` multiplier4 thread=2 median (samples 1-3): end-to-end
594.1709836 s (wrapper 594.2089 s); summed all-level exhaustive 479,603 ms;
summed all-level manifest 63,823 ms. Peak working set median 1,624,854,528 B
(max 1,625,137,152 B), peak private median 1,666,154,496 B. Top-level `PHASE
exhaustive` speedup is 1.93x versus the retained baseline serial median and
1.86x versus this layer's own current thread=1 median. Same-setting
end-to-end is 44.00% faster than the current thread=1 median (594.1709836 s
vs 1061.0744303 s), and separately 42.67% faster than the retained baseline
thread=2 median. Both ratios clear the >=1.5x Gate 1 threshold with more than
20% margin over 1.5x, so the Opus binding adjudication's escalation clause
(additional t2 samples if the margin over 1.5x is under 20%) is not
triggered; no further `threads=2` samples are required and this median is the
basis for the Gate 1 decision at thread=2. The earlier invalid-sample's
~1.991x memory ratio (see "Measurement method" above) is consistent with —
and corroborates — two overlapping single-world compiles rather than any
genuine per-worker memory blowup at legitimate thread counts, since every
legitimate thread=1/2/auto sample above stays within a few percent of the
single-world peak working set.

Current `1e62683` multiplier4 auto sample 1 (exact single test), raw all-level
detail: leaf/intermediate/top exhaustive 155/15,334/174,128 ms (sum 189,617
ms); manifest 331/3,399/37,513 ms (sum 41,243 ms); end-to-end 279.5675934 s
(wrapper 279.7165 s); peak working set 1,636,679,680 B, peak private
1,678,127,104 B; case/candidate/world/manifest/timing-graph fingerprints and
quality identical to the retained baseline. Requested auto budget was 12.
Actual granted workers: leaf exhaustive/manifest 1/7; intermediate 12/4; top
11/4 (top per-worker 96,658,272 B makes the 1-GiB auto ceiling 11, one below
the requested 12). per_worker_bytes 96,658,272, budget 1,073,741,824 B.

Preliminary (single auto sample, not yet a median — two more `auto` samples
are still required before this row's median can be computed): the top-level
`PHASE exhaustive` time is roughly 4.88x faster than the retained
pre-parallelization baseline median and roughly 4.70x faster than this
layer's own current thread=1 median; same-setting end-to-end is about 71.40%
lower than the retained baseline auto median. Both preliminary ratios already
clear the >= 1.5x Gate 1 threshold with large margin.

Current `1e62683` multiplier4 auto sample 2 (exact single test), raw all-level
detail: leaf/intermediate/top exhaustive 154/14,950/174,208 ms (sum 189,312
ms); manifest 331/3,263/37,552 ms (sum 41,146 ms); end-to-end 279.1949518 s
(wrapper 279.4364 s); peak working set 1,627,254,784 B, peak private
1,669,943,296 B. Worker diagnostics (requested auto budget 12, granted leaf
exhaustive/manifest 1/7, intermediate 12/4, top 11/4, per_worker_bytes
96,658,272, budget 1,073,741,824 B, 1-GiB auto ceiling 11) and all
case/candidate/world/manifest/timing-graph fingerprints and quality exactly
match auto sample 1.

Current `1e62683` multiplier4 auto sample 3 (exact single test), raw all-level
detail: leaf/intermediate/top exhaustive 156/15,333/174,541 ms (sum 190,030
ms); manifest 329/3,072/37,110 ms (sum 40,511 ms); end-to-end 280.2311574 s
(wrapper 280.4929 s); peak working set 1,624,862,720 B, peak private
1,670,864,896 B. Worker diagnostics (requested auto budget 12, granted leaf
exhaustive/manifest 1/7, intermediate 12/4, top 11/4, per_worker_bytes
96,658,272 B, budget 1,073,741,824 B, 1-GiB auto ceiling 11) and all
case/candidate/world/manifest/timing-graph fingerprints and quality exactly
match auto samples 1-2.

Current `1e62683` multiplier4 auto median (samples 1-3): end-to-end
279.5675934 s (wrapper 279.7165 s); summed all-level exhaustive 189,617 ms;
summed all-level manifest 41,146 ms; peak working set median 1,627,254,784 B
(max 1,636,679,680 B); peak private median 1,670,864,896 B (max
1,678,127,104 B). Top-level `PHASE exhaustive` speedup is 4.88x versus the
retained baseline thread=1 median and 4.70x versus this layer's own current
thread=1 median; same-setting end-to-end speedup is 3.50x versus the retained
baseline auto median. Identity is exact across all three samples and every
sweep matched the requested worker diagnostics. Both ratios clear the >=1.5x
Gate 1 threshold with large margin, and the median (not a single sample) is
now the basis for the Gate 1 decision at auto.

## Raw samples — guard fixtures (one sample per thread setting)

Commands (same shape, `REDA_EXTRA_CIRCUITS` set per case):

```powershell
$env:REDA_PHASE_TIMING='1'
$env:REDA_EXTRA_CIRCUITS='ripple_adder8'   # then 'alu8'
$env:REDA_CERT_THREADS='1'   # then '2', then Remove-Item Env:REDA_CERT_THREADS for auto
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan -- --exact --ignored --nocapture --test-threads=1
```

| Case | Threads | `PHASE exhaustive` (top) | `WORK exhaustive_workers` | End-to-end | Peak working set | Fingerprint | Quality |
|---|---|---:|---:|---:|---:|---|---|
| `ripple_adder8` | 1 | 0 ms (leaf 153 ms) | requested=1 explicit=Some(1) granted=1 | 99.173844 s (wrapper 99.3315 s) | 680,456,192 B (peak private 693,702,656 B) | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` | 608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678 |
| `ripple_adder8` | 2 | 0 ms (leaf 154 ms) | leaf exhaustive granted=1; manifest budget/granted=2; top manifest requested=2 explicit=Some(2) granted=2 | 62.1374985 s (wrapper 62.3669 s) | 680,841,216 B (peak private 692,834,304 B) | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` | 608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678 |
| `ripple_adder8` | auto | 0 ms (leaf 153 ms) | requested=12 explicit=None granted=12; actual leaf exhaustive/manifest=1/7; top manifest=8 | 39.6760567 s (wrapper 39.8853 s) | 681,123,840 B (peak private 696,365,056 B) | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` | 608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678 |
| `alu8` | 1 | 0 ms (alu4 0 ms; leaf 8,233 ms) | requested=1 explicit=Some(1) granted=1 | 341.3732195 s (wrapper 341.6066 s) | 1,633,894,400 B (peak private 1,674,346,496 B) | `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02` | 972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204 |
| `alu8` | 2 | 0 ms (alu4 0 ms; leaf 4,546 ms) | requested=2 explicit=Some(2) granted=2 | 208.7548276 s (wrapper 208.7740 s) | 1,632,575,488 B (peak private 1,670,184,960 B) | `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02` | 972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204 |
| `alu8` | auto | 0 ms (alu4 0 ms; leaf 1,746 ms) | requested=12 explicit=None; leaf budget granted=12, actual exhaustive/manifest workers=12/3; alu4 manifest workers=5; top memory ceiling/granted=10, manifest actual=9 | 127.3211882 s (wrapper 127.4063 s) | 1,639,464,960 B (peak private 1,679,282,176 B) | `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02` | 972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204 |

Guard rows exist to confirm no representative case regresses more than 5% and
that fingerprints/quality stay identical across thread settings; they are not
inputs to the primary PHASE speedup computation.

Current `1e62683` `ripple_adder8` thread=1 guard (exact single test): PASS;
end-to-end 99.173844 s (wrapper 99.3315 s); peak working set 680,456,192 B,
peak private 693,702,656 B; leaf/top exhaustive 153/0 ms; leaf/top manifest
1,290/79,013 ms; case fingerprint
`b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`, candidate
`a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2`, world
`700e847799b6ec9f1512deddd61537777a6a33ce3d4a028f37a73ee5b88bc0e7`; quality
608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678;
every sweep printed requested=1 explicit=Some(1) granted=1. Matches the
retained baseline thread=1 guard's fingerprints and quality exactly; end-to-end
is 2.6% faster than the retained baseline thread=1 guard (101.8152310 s),
well within the 5% no-regression gate.

Current `1e62683` `ripple_adder8` thread=2 guard (exact single test): PASS;
end-to-end 62.1374985 s (wrapper 62.3669 s); peak working set 680,841,216 B,
peak private 692,834,304 B; leaf/top exhaustive 154/0 ms; leaf/top manifest
739/42,099 ms. Leaf exhaustive's actual worker count stayed at 1 (below the
parallel-worthwhile threshold), while manifest's budget/granted was 2 and top
manifest printed requested=2 explicit=Some(2) granted=2. Identity (case
`b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`) and quality
(608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay 678)
exactly match its own thread=1 row above. End-to-end is 2.99% faster than the
retained baseline thread=2 guard (64.0509431 s), well within the 5%
no-regression gate.

Current `1e62683` `alu8` thread=1 guard (exact single test): PASS; end-to-end
341.3732195 s (wrapper 341.6066 s); peak working set 1,633,894,400 B, peak
private 1,674,346,496 B; leaf/alu4/top exhaustive 8,233/0/0 ms; manifest
1,934/41,725/233,262 ms; case
`856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02`, candidate
`19ea67df99e616c8b8789fc8e1119868c0ae588c2a8653d798bfa9b2e6f054ee`, world
`4d4cd3860456b32c1b47c8855959874ae9fc3c99871dcbaa10f20eba7c5c879a`; quality 972
ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204; every
sweep printed requested=1 explicit=Some(1) granted=1. Matches the retained
baseline thread=1 guard's fingerprints and quality exactly; end-to-end is
2.19% faster than the retained baseline thread=1 guard (349.0261563 s), well
within the 5% no-regression gate.

Current `1e62683` `alu8` thread=2 guard (exact single test): PASS; end-to-end
208.7548276 s (wrapper 208.7740 s); peak working set 1,632,575,488 B, peak
private 1,670,184,960 B; leaf/alu4/top exhaustive 4,546/0/0 ms; manifest
1,012/22,206/124,564 ms; case
`856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02`, candidate
`19ea67df99e616c8b8789fc8e1119868c0ae588c2a8653d798bfa9b2e6f054ee`, world
`4d4cd3860456b32c1b47c8855959874ae9fc3c99871dcbaa10f20eba7c5c879a`; quality
972 ticks / 213,833 blocks / occupied volume 2,902,664 / static delay 1204;
every sweep printed requested=2 explicit=Some(2) granted=2, leaf exhaustive
actual workers=2. Matches its own thread=1 row's fingerprints and quality
exactly; end-to-end is 3.70% faster than the retained baseline thread=2 guard
(216.7815886 s), well within the 5% no-regression gate.

Current `1e62683` `ripple_adder8` auto guard (exact single test): PASS;
end-to-end 39.6760567 s (wrapper 39.8853 s); peak working set 681,123,840 B,
peak private 696,365,056 B; leaf/top exhaustive 153/0 ms; manifest
332/20,334 ms; requested auto budget was 12, explicit=None, granted=12;
actual leaf exhaustive/manifest workers 1/7, top manifest workers 8. Identity
(case `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`) and
quality (608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay
678) exactly match the thread=1/thread=2 guard rows above. **Flag:** this
end-to-end is 12.01% *slower* than the retained-foundation auto median cited
for this case (35.4198535 s; see "Foundation values reused as-is" above),
which exceeds the plan's 5% no-representative-case-regression gate. The two
values are not a like-for-like same-code comparison — the foundation figure
predates this layer's exhaustive parallelization and worker-budget
coordination entirely — but the plan's stated gate compares same thread
setting regardless of code provenance, so this regression must be resolved
(root-caused and either fixed or explicitly waived) by Opus review before
Gate 1 can be declared met for the auto setting; it is not resolved by this
report and is called out here rather than suppressed.

**Open item, not yet decided:** this is a single `ripple_adder8` auto sample,
not a median. Two additional exact `ripple_adder8` auto samples are still
required; the three-sample median (not this one sample) will decide whether
the case actually breaches the plan's <=5% no-regression gate, because at
this same case both thread=1 (-2.6%) and thread=2 (-2.99%, i.e. faster) show
no regression while only this single auto sample regresses (+12.01%). Until
that median is in hand, this row is evidence, not a Gate-1 verdict, and
retention is not decided here.

Current `1e62683` `alu8` auto guard (exact single test): PASS; end-to-end
127.3211882 s (wrapper 127.4063 s); peak working set 1,639,464,960 B, peak
private 1,679,282,176 B; leaf/alu4/top exhaustive 1,746/0/0 ms; manifest
765/12,054/56,393 ms; case
`856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02`, candidate,
world and quality exactly match the `alu8` thread=1/thread=2 guard rows above.
Requested auto budget was 12, explicit=None; leaf budget granted=12 with
actual exhaustive/manifest workers 12/3; alu4-level manifest workers=5; top
memory ceiling/granted=10, with actual manifest workers=9. Derived end-to-end
speedup versus the current thread=1 guard (341.3732195 s) is about 2.68x
(341.3732195 / 127.3211882 ≈ 2.681x). No foundation auto citation exists at
per-sample granularity for this case to compare against, so no regression
flag applies to this row.

## Memory bound check

```text
PHASE exhaustive speedup (t2 vs t1)   = median(t1 exhaustive) / median(t2 exhaustive)   = 1.86x (also 1.93x versus retained baseline serial median)
PHASE exhaustive speedup (auto vs t1) = median(t1 exhaustive) / median(auto exhaustive) = 4.70x (also 4.88x versus retained baseline)
end-to-end change (t2 vs t1)          = (median(t2 end-to-end) - median(t1 end-to-end)) / median(t1 end-to-end) = -44.00% (594.1709836 s vs 1061.0744303 s); separately, 42.67% faster than the retained baseline thread=2 median
end-to-end change (auto vs t1)        = (median(auto end-to-end) - median(t1 end-to-end)) / median(t1 end-to-end) = -73.65% (279.5675934 s vs 1061.0744303 s); versus retained baseline auto median, end-to-end speedup is 3.50x
```

Gate 1 (this layer) requires the first line >= 1.5x. Both t2 lines above are
final medians (three samples each) and clear >=1.5x with more than 20%
margin, so the Opus binding adjudication's escalation clause (additional t2
samples if the margin over 1.5x is under 20%) is not triggered; no further
`multiplier4` t2 samples are required.

## Memory bound check

`per_worker_bytes` (from the `WORK sweep_budget` diagnostic) already includes
`WORLD_COPY_HEADROOM` — it is not a raw per-world size to be multiplied by
headroom again. The bound check instead compares the *increment* the model
predicts for `N` workers against the *increment* actually observed relative to
the N=1 (single-world) peak, and backs out the headroom the data implies:

```text
per_worker_bytes (N workers)     = 96,658,272 B (from WORK sweep_budget diagnostic; includes WORLD_COPY_HEADROOM)
world_volume                     = 6,041,142 (3771x6x267, from WORK sweep_budget diagnostic)
actual worker count N            = 11 (top-level auto ceiling, from WORK exhaustive_workers)

model_increment(N)    = (N - 1) * per_worker_bytes                                       = 10 * 96,658,272 B = 966,582,720 B
observed_increment(N) = peak_working_set(auto max) - peak_working_set(thread=1 max)      = 1,636,679,680 B - 1,625,018,368 B = 11,661,312 B
implied_headroom      = observed_increment(N) / ((N - 1) * 4 * world_volume)             = 11,661,312 B / (10 * 24,164,568 B) ≈ 0.0483
within model?         = observed_increment(N) <= model_increment(N)                      = yes (11,661,312 B <= 966,582,720 B)
```

The conservative max peak increment over the current thread=1 max is
11,661,312 B against the 10-extra-worker model allowance of 966,582,720 B; the
implied dense-world headroom is about 0.0483, far below the retained
`WORLD_COPY_HEADROOM=4` already folded into `per_worker_bytes`. The byte
ceiling is adequate and memory does not grow with host core count. No double
counting: `4 * world_volume` above is the raw per-world byte size
(`size_of::<u32>() * volume`) before headroom, not `per_worker_bytes` itself.

`4 * world_volume` is the raw per-world byte size (`size_of::<u32>() *
volume`) before headroom is applied. Never multiply `per_worker_bytes` by
`WORLD_COPY_HEADROOM` a second time. If `implied_headroom` exceeds the
constant already folded into `per_worker_bytes`, that measured constant must
be raised to match reality rather than reducing worker count ad hoc (plan
Task 5 step 1).

### Memory-budget cap enforcement proof (`REDA_CERT_MEMORY_BYTES=1`)

The increment check above shows the observed increment stays *under* the
model's allowance at a generous byte budget; it does not by itself prove the
cap is actually enforced when the budget is deliberately starved. This
fixture closes that gap: `ripple_adder8` was run with
`REDA_CERT_THREADS` unset (auto) and `REDA_CERT_MEMORY_BYTES=1` — a
byte budget so small that the model must fall back to a single granted
worker regardless of the auto-requested budget of 12.

Current `1e62683` `ripple_adder8` auto + `REDA_CERT_MEMORY_BYTES=1` (exact
single test): PASS; end-to-end 99.3028581 s (wrapper 99.3881 s); peak working
set 679,915,520 B, peak private 691,904,512 B; leaf/top exhaustive 154/0 ms;
manifest 1,280/79,093 ms. Every applicable sweep printed
`memory_budget_bytes=1 ceiling=1 requested=12 explicit=None granted=1`;
actual exhaustive/manifest workers=1. Identity (case
`b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`) and
quality (608 ticks / 70,603 blocks / occupied volume 1,123,332 / static delay
678) exactly match every other `ripple_adder8` row above.

This confirms the memory-budget cap is a real, enforced lower bound on
granted workers, not just an unreached theoretical ceiling: at
`memory_budget_bytes=1`, `ceiling` collapses to 1 and `granted` follows it to
1 even though `requested=12` (the same auto request as the unconstrained auto
guard above), and the resulting end-to-end (99.3028581 s) and peak
working/private set are consistent with this case's thread=1 row (99.173844 s
/ 680,456,192 B), not with its 12-requested auto row (39.6760567 s /
681,123,840 B) — i.e. the process actually ran single-worker under the
starved budget rather than merely reporting a low ceiling while still running
parallel.

## `cargo test --lib` wall time: aggregate before/after this layer

Command, run at `f899d6c` (before, Task 1 only: shared ordered-chunk helper,
exhaustive not yet routed through it) and at `HEAD` (after, all of this
layer's production and test commits applied):

```powershell
cargo test --lib
```

| State | Wall time |
|---|---:|
| Before this layer (`f899d6c`) | TBD_MEASURED |
| After this layer (`HEAD`) | TBD_MEASURED |

This is an **aggregate, confounded** comparison, not an isolated lock cost:
`a6588b9` already routes exhaustive through the same guarded shared helper
that carries the process-wide sweep lock, so the before/after delta reflects
the combined effect of exhaustive parallelism, the shared lock now covering
exhaustive as well as manifest, and Task 3's worker-budget coordination
together. It shows whether independent-test-compile serialization became
visible at the `cargo test --lib` scale, not the lock's cost in isolation.

## Complete verification (run serially, in order)

```powershell
cargo test --lib
cargo test --release --lib every_extra_circuit -- --ignored
cargo test --release --lib every_large_circuit -- --ignored
cargo test --release --test fragment_synth_acceptance -- --nocapture
$parallelCertDir = Join-Path $env:TEMP ("reda-parallel-cert-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $parallelCertDir | Out-Null
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output "$parallelCertDir/acceptance.json" --shipping-source "$parallelCertDir/shipping_config.rs" --shuffle-seed 0x5245444120260831
cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --nocapture
cargo clippy --lib --tests
git diff --check
```

| Command | Result | Wall time |
|---|---|---:|
| `cargo test --lib` | TBD_MEASURED | TBD_MEASURED |
| `every_extra_circuit` (release, ignored) | TBD_MEASURED | TBD_MEASURED |
| `every_large_circuit` (release, ignored) | TBD_MEASURED | TBD_MEASURED |
| `fragment_synth_acceptance` (release) | TBD_MEASURED | TBD_MEASURED |
| `fragment_acceptance` binary run | may cite Task 4, see condition below | may cite Task 4 |
| pinned seven-segment IO contract | may cite Task 4, see condition below | may cite Task 4 |
| `cargo clippy --lib --tests` | TBD_MEASURED | — |
| `git diff --check` | TBD_MEASURED | — |

The `fragment_acceptance` binary run and the pinned seven-segment IO contract
may be cited from Task 4 instead of rerun **only if both hold**: (a) `git diff
685dfa5..HEAD` remains test-only (verified at draft time: it touches only
`src/compile/fragment_synth/seed.rs`, all inside `#[cfg(test)]`), and (b) the
temporary `WORK sweep_budget` diagnostic and any other temporary instrumentation
have been reverted before the final report. If Opus review requires any
production change after `685dfa5`, both commands must be rerun against the
post-fix state before citing them. Prior committed Task 4 evidence already
covers the full 1/2/auto correctness matrix and is cited rather than
re-derived here (`cargo test --release --lib certification_thread_counts --
--ignored --nocapture --test-threads=1`: 3 passed, 0 failed, 997 filtered out,
12,331.98 s; see
`.superpowers/sdd/2026-09-07-portable-parallel-certification/task-4-report.md`
"Controller acceptance evidence"). This report's own samples above target the
PHASE-level speedup gate that Task 4 did not measure (Task 4 explicitly adds
no timing assertion).

## Cross-machine scope

This host (12 logical processors, 68,664,922,112 bytes physical memory)
establishes the layer's retention gate. Per the plan and spec, this report
must not claim cross-machine scaling until the same release matrix has been
run and recorded on a second, distinct machine.

## Opus review

Fresh Opus 5 review pending. Required explicit findings: ordered error/panic
reduction, cap accounting, thread-budget restoration, nested oversubscription,
memory, pinned IO, benchmark arithmetic. `TBD_MEASURED` (review outcome,
findings, fix commits if any).

## Retention decision

`TBD_MEASURED` — KEEP only if every gate at the top of the plan passes
(>=1.5x `PHASE exhaustive` on `multiplier4`, identical outputs at 1/2/auto,
no representative case >5% slower, bounded peak memory); otherwise revert all
production commits from this layer (`f899d6c`, `a6588b9`, `685dfa5`) and keep
only the test-only Task 4 commits that remain valid against the reverted
state, per plan judgment at revert time.

## Cleanup before final commit

- Revert the temporary `WORK sweep_budget` diagnostic eprintln and any
  temporary fingerprint-capture test edits in `certification.rs` / `seed.rs`
  once `per_worker_bytes` and the memory ceiling have been recorded above.
- Re-run `git diff --check` and confirm the working tree is clean except for
  this report file before commit.
