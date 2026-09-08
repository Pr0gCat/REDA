# Portable Parallel Certification — Benchmark and Retention Report

Status: **FINAL — COMPLETE. Retention decision: KEEP.** Primary and guard
PHASE-gate matrices are measured, including the `ripple_adder8`/`multiplier4`
manifest-worker regression found at parent `1e62683`, root-caused and fixed
by commit `d43e62f` (re-reviewed and approved at `94e171b`), and a second,
independently-found and independently-reviewed test-only fix `b9e0c63` for a
lock-after `cargo test --lib` flake. The layer range was then extended to
`61ffdb1..f22bf8e`: commit `0f5a2db` resolved Round 4's I1 and I3 findings
(honest worker-policy test naming, real `FunctionalMismatch` refusal
fixture) and corrected I2's wording, and commit `f22bf8e` fully corrected I2
(unmeasured range is "below 68 transitions," not "2-7") and pinned the new
fixture's worker counts; **the current authoritative `HEAD` is `f22bf8e`**,
test/comment-only with no production semantics change. Lock-before/
lock-after `cargo test --lib` are measured; a full-suite run at `0f5a2db`
(928 passed / 0 failed / 74 ignored, libtest 735.48 s, production-equivalent
for `f22bf8e`) and a focused certification-module rerun at `f22bf8e` itself
(23 passed / 0 failed / 979 filtered out) are both recorded; every row of the
"Complete verification" table is measured, including `cargo clippy --lib
--tests` at `f22bf8e` (exit 0, cargo 7.20 s / wrapper 7.3466 s, 27 lib + 31
lib-test pre-existing warnings, not warning-free) and a clean `git diff
--check`. Four rounds of Opus review have all completed: Round 4
(whole-layer, scoped at the time to `f899d6c`..`b9e0c63`) found one
report-accuracy correction (C1) and six Important findings (I1-I6), all
addressed by corrections applied directly in this report; a separate,
independent review of `0f5a2db` (Round not separately numbered) returned SPEC
PASS / CODE QUALITY APPROVED; Round 5 (`f22bf8e` scoped) returned **SPEC
PASS, CODE QUALITY PASS, RETAIN as-is**, confirming all three I1-I3 remedies,
no production behavior change, sound 32-vector arithmetic/type inference, and
the 23/23 test evidence; and **Round 6, the final independent exact-range
review across the full `61ffdb1..f22bf8e` range, returned SPEC PASS, CODE
QUALITY PASS, KEEP, no Critical or Important findings.** No Opus review item
remains outstanding. This file records the exact commands, already-verified
facts and the measured results gathered.

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

- Date: 2026-09-08 (report authored and all measurements below taken).
- Worktree: `topology-aware-seed-v2-6f8f7e`.
- Host logical processors: 12.
- Host physical memory: 68,664,922,112 bytes.
- Build: Cargo release profile, built before samples (build time excluded from
  all reported wall times).
- Timing: `REDA_PHASE_TIMING=1`.
- Retained foundation revision (comparison baseline): `95b6b9d` (see
  `docs/superpowers/reports/2026-09-07-portable-performance-foundation.md`).
- Layer commit range under test: `61ffdb1..f22bf8e` — `61ffdb1` (`docs: plan
  portable parallel certification`) is the parent commit immediately before
  Task 1's first commit and is the diff/range boundary, not itself a Task 5
  production or test change; `f22bf8e` is the current authoritative `HEAD`.
  Commits in the range: `f899d6c` (Task 1, ordered chunk helper),
  `a6588b9` (Task 2, `perf: parallelize exhaustive certification`),
  `685dfa5` (Task 3, `perf: coordinate compile-wide worker budgets`),
  `611349a`..`1e62683` (Task 4, thread-count matrix tests: `test: cover
  certification thread-count matrix`, `test: harden certification thread
  matrix`, `test: reject empty certification matrix`), `d43e62f` (Task 5 fix,
  `perf: restore manifest sweep parallelism`, applied after the `ripple_adder8`
  auto regression below was found at parent `1e62683`), `94e171b` (Task 5,
  `test: bind manifest worker policy to production`, a behaviour-only test
  addition on top of `d43e62f` with no production semantic change — despite
  its name, it exercises the `certification_workers` policy constants only,
  not the manifest-sweep callsite itself; see "Opus review" correction I1
  below, resolved by `0f5a2db`), `b9e0c63` (Task 5, `test: drop wall-clock
  deadline from sweep lock handoff`, a test-only fix for the flaky `cargo
  test --lib` failure found by the lock-after diagnostic below), `0f5a2db`
  (Task 5, `test: cover exhaustive functional mismatch and correct
  worker-policy claims`, test/comment-only: resolves I1/I3, corrects I2's
  wording), `f22bf8e` (Task 5, `docs: bound the manifest threshold claim to
  measured cases; pin test worker counts`, test/comment-only, "No production
  semantics change" per its own commit message — **the current authoritative
  `HEAD` under test**).
- Temporary instrumentation was present in the working tree at draft time
  (`src/compile/fragment_synth/certification.rs`, `src/compile/fragment_synth/seed.rs`):
  a `REDA_PHASE_TIMING`-gated `WORK sweep_budget` diagnostic line reporting
  `world`, `volume`, `per_worker_bytes`, `memory_budget_bytes`, `ceiling`,
  `requested`, `explicit` and `granted`, plus fingerprint capture in `seed.rs`.
  It existed only to source the `per_worker_bytes` / memory-ceiling rows
  below and **has since been reverted** (confirmed by the current `git diff
  --check` and `cargo clippy --lib --tests` results in "Complete
  verification" below, both clean at `HEAD`); no further revert action is
  outstanding for it.

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
(`src/compile/fragment_synth/seed.rs:4887`, checked against current `HEAD`
source; the function itself is declared at line 4888 — this line reference
had drifted in an earlier draft and is corrected here), is not unique: it
also matches
`compile::fragment_synth::seed::tests::every_hierarchical_circuit_agrees_across_certification_thread_counts`,
Task 4's release-only, serial thread-count-matrix test, declared at
`src/compile/fragment_synth/seed.rs:5185` (`#[ignore]` message at line 5184;
this line reference is likewise corrected against current `HEAD` source).
libtest's substring filter
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
| auto | Median | 174,208 ms (leaf 155 ms; intermediate 15,333 ms; sum 189,617 ms) | requested=12; leaf exhaustive/manifest granted=1/7; intermediate granted=12/4; top granted=11/4 | 279.5675934 s (wrapper 279.7165 s) | 1,627,254,784 B (max 1,636,679,680 B); peak private median 1,670,864,896 B (max 1,678,127,104 B) | 96,658,272 B | budget 1,073,741,824 B; 1-GiB auto ceiling 11 | matches retained baseline exactly | matches retained baseline exactly |

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

**Correction (I4, from the verified whole-layer Opus review): this thread=2
row is historical, pre-fix evidence, superseded for the retention decision.**
Everything in this paragraph was measured at parent revision `1e62683`,
*before* `d43e62f`'s manifest-worker-policy fix. Unlike thread=1 (where
`REDA_CERT_THREADS=1` always requests exactly one worker and is therefore
unaffected by the manifest-threshold constant either way, so the `1e62683`
thread=1 numbers above remain valid evidence at `HEAD`), thread=2's manifest
worker grants can differ before and after `d43e62f` — the auto-setting
`multiplier4` re-verification above already shows the fix changing manifest
worker grants materially (leaf/intermediate/top manifest workers rose from
7/4/4 to 12/12/11 at auto). This thread=2 arithmetic is **not** re-presented
as `HEAD` evidence, and no `HEAD` thread=2 number is invented here to replace
it. The Gate 1 decision at this layer's retained `HEAD` is instead decided by
the "Final `HEAD` (`94e171b`) `multiplier4` auto re-verification" section
above (still valid at the current `HEAD` `b9e0c63`, since `94e171b`/`b9e0c63`
are behaviour-only/test-only on top of `d43e62f`) together with the valid
`HEAD`/thread=1 comparisons cited there, not by the pre-fix thread=2 numbers
below.

**M1 (final exact-range Opus review): this thread=2 evidence gap is closed —
the pre-fix `1e62683` thread=2 rows below are in fact valid `HEAD`-equivalent
evidence for the manifest sweeps this report actually measured.** At
`REDA_CERT_THREADS=2` (`requested=2`), the pre-fix shared threshold policy
grants `min(2, transitions/8)` and the post-`d43e62f` policy grants
`min(2, transitions/1) = min(2, transitions)`; these two formulas agree
(both grant 2) whenever `transitions >= 16`, since `transitions/8 >= 2` at
that point under either policy's integer-division floor. Every manifest
sweep measured in the `multiplier4` thread=2 row below has an item count of
16 or more, so the pre-fix and post-fix policies grant identically 2 workers
for it — the `1e62683` thread=2 numbers below are therefore not merely
historical color, they are valid `HEAD`-equivalent evidence for these
specific sweeps, closing the gap the I4 correction above otherwise left open.
This does not change the I4 correction's core instruction (no `HEAD`
thread=2 number is *invented*, and the auto matrix remains the primary Gate 1
evidence at `HEAD`); it only establishes that the thread=2 numbers already on
record are not invalidated by `d43e62f` for the cases actually measured.

Current `1e62683` multiplier4 thread=2 median (samples 1-3), retained as
historical evidence of the pre-fix state only: end-to-end 594.1709836 s
(wrapper 594.2089 s); summed all-level exhaustive 479,603 ms;
summed all-level manifest 63,823 ms. Peak working set median 1,624,854,528 B
(max 1,625,137,152 B), peak private median 1,666,154,496 B. Top-level `PHASE
exhaustive` speedup is 1.93x versus the retained baseline serial median and
1.86x versus this layer's own current thread=1 median (both pre-fix, at
`1e62683`). Same-setting
end-to-end is 44.00% faster than the current thread=1 median (594.1709836 s
vs 1061.0744303 s), and separately 42.67% faster than the retained baseline
thread=2 single sample (1036.3501146 s; that baseline setting has only one
sample, not a median — see "Raw baseline `95b6b9d` samples" above). Both
pre-fix ratios cleared the >=1.5x Gate 1 threshold with more than
20% margin over 1.5x at `1e62683`, so at that revision the Opus binding
adjudication's escalation clause (additional t2 samples if the margin over
1.5x is under 20%) was not triggered; no further `threads=2` samples were
required at `1e62683`. This historical result is superseded, not the basis
for the Gate 1 decision at the retained `HEAD`. The earlier invalid-sample's
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
279.5675934 s (wrapper 279.7165 s); top-only `PHASE exhaustive` median
174,208 ms (leaf 155 ms; intermediate 15,333 ms) — **correction:** an earlier
draft of this row misreported this as 174,128 ms (sample 1's own top figure,
reused instead of the true independently-sorted median of the three samples'
174,128/174,208/174,541 ms top values; 174,208 ms is correct); summed
all-level exhaustive 189,617 ms; summed all-level manifest 41,146 ms; peak
working set median 1,627,254,784 B (max 1,636,679,680 B); peak private median
1,670,864,896 B (max 1,678,127,104 B). Top-level `PHASE exhaustive` speedup
(using the corrected 174,208 ms top-only median) is 4.88x versus the retained
baseline thread=1 median (850,937 ms) and 4.71x versus this layer's own
current thread=1 median (821,137 ms) — the second ratio moves from the
previously reported 4.70x to 4.71x under the corrected figure; same-setting
end-to-end speedup is 3.50x versus the retained baseline auto median.
Identity is exact across all three samples and every sweep matched the
requested worker diagnostics. Both ratios clear the >=1.5x Gate 1 threshold
with large margin, and the median (not a single sample) is now the basis for
the Gate 1 decision at auto.

### Final `HEAD` (`94e171b`) `multiplier4` auto re-verification

`94e171b` (`test: bind manifest worker policy to production`) is a
behaviour-only test/constant-binding commit layered on top of `d43e62f` with
no production semantic change. Because `d43e62f`'s manifest-threshold fix (see
"Raw samples — guard fixtures" below) also changes `multiplier4`'s manifest
worker grants, the full exact `multiplier4` auto three-sample matrix was
re-run at the actual retained `HEAD` (`94e171b`) rather than citing the
pre-fix `1e62683` auto rows above as final.

| Sample | End-to-end | Wrapper | Peak working set | Peak private | `route_nets` (leaf/inter/top) | Exhaustive (leaf/inter/top, sum) | Manifest (leaf/inter/top, sum) |
|---:|---:|---:|---:|---:|---|---|---|
| 1 | 270.3840360 s | 270.7970 s | 1,628,512,256 B | 1,672,196,096 B | 575/2,916/42,554 ms | 159/15,868/178,488 ms (sum 194,515 ms) | 274/2,054/24,704 ms (sum 27,032 ms) |
| 2 | 271.5298186 s | 271.5983 s | 1,622,888,448 B | 1,672,114,176 B | 566/2,936/42,360 ms | 160/14,984/179,836 ms (sum 194,980 ms) | 289/2,015/25,682 ms (sum 27,986 ms) |
| 3 | 272.4599747 s | 272.7088 s | 1,624,166,400 B | 1,667,551,232 B | 568/2,910/42,297 ms | 159/15,176/181,600 ms (sum 196,935 ms) | 276/2,087/24,629 ms (sum 26,992 ms) |
| Median | 271.5298186 s | 271.5983 s | 1,624,166,400 B (max 1,628,512,256 B) | 1,672,114,176 B (max 1,672,196,096 B) | n/a (per-sample only) | top 179,836 ms; sum 194,980 ms | top 24,704 ms; sum 27,032 ms |

Every sample's worker diagnostics: leaf/intermediate/top exhaustive granted
workers = 1/12/11; leaf/intermediate/top manifest granted workers = 12/12/11
(manifest workers rose from the pre-fix `1e62683` auto grants of 1/7, 12/4,
11/4 — direct confirmation that `d43e62f` restores manifest parallelism for
`multiplier4` as well as `ripple_adder8`). Quality is 1,039 ticks / 124,948
blocks in all three samples, matching every prior `multiplier4` row in this
report.

Speedup at `HEAD` (`94e171b`):
- End-to-end vs the retained-foundation auto median (977.6524666 s):
  977.6524666 / 271.5298186 ≈ 3.60x faster.
- **Summed all-level** (leaf+intermediate+top) exhaustive vs the retained
  baseline serial summed median (924,859 ms): 924,859 / 194,980 ≈ 4.74x
  faster.
- **Summed all-level** exhaustive vs this layer's own current thread=1 summed
  median (891,957 ms, unaffected by the manifest-only fix): 891,957 / 194,980
  ≈ 4.57x faster.

**Correction (I5, from the verified whole-layer Opus review): top-only and
summed-all-level are different denominators and must be labeled, not mixed.**
The two ratios directly above use the **summed all-level** (leaf +
intermediate + top) `PHASE exhaustive` figure, 194,980 ms, as the
denominator — they are correctly labeled "summed all-level" above and are
retained as-is. For direct comparability with the **top-only** ratios
computed elsewhere in this report (e.g. the pre-fix `1e62683` auto median's
"Top-level `PHASE exhaustive` speedup" figures and the "Speedup arithmetic"
section below, both of which use the **top-only** exhaustive time as the
denominator), here are the equivalent **top-only** ratios at `HEAD`, using
the `HEAD` auto median's top-only exhaustive time (179,836 ms) against the
same top-only baseline/thread=1 numerators used elsewhere in this report
(850,937 ms and 821,137 ms respectively):
- **Top-only** exhaustive vs this layer's own current thread=1 top-only
  median (821,137 ms): 821,137 / 179,836 ≈ 4.57x faster.
- **Top-only** exhaustive vs the retained baseline serial top-only median
  (850,937 ms): 850,937 / 179,836 ≈ 4.73x faster.

The top-only and summed-all-level ratios happen to agree at ≈4.57x versus
thread=1 here, but diverge slightly versus the retained baseline (4.73x
top-only vs 4.74x summed) — both are valid, but only when each ratio's
numerator and denominator use the same (top-only or summed-all-level)
convention; do not divide a top-only numerator by a summed-all-level
denominator or vice versa.

All ratios above clear the >=1.5x Gate 1 threshold with large margin, and no
representative case regresses. Memory bound re-check: the max peak working
set across these three `HEAD` samples (1,628,512,256 B) is an increment of
3,493,888 B over the current thread=1 max recorded above (1,625,018,368 B;
thread=1's own worker count and per-worker memory model are unaffected by a
manifest-only fix), far below the 10-extra-worker model allowance of
966,582,720 B computed earlier in this section — the byte ceiling remains
adequate at `HEAD`.

Fingerprints were not printed for these three `HEAD` samples because the
temporary diagnostic fingerprint capture (see Environment above) had already
been reverted; quality identity (1,039 ticks / 124,948 blocks) is the only
per-run identity signal recorded here, and case/candidate/world identity for
the retained revision is covered by the committed Task 4 acceptance matrix,
not by this benchmark row. This `HEAD` matrix supersedes the pre-fix
`1e62683` `multiplier4` auto rows above for the Gate 1 decision at the
revision actually being retained; those `1e62683` rows remain valid evidence
of the regression that motivated the `d43e62f` fix and are not discarded.

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

**Resolution: the required three-sample median was taken, confirmed the
regression as genuine, and the underlying defect has since been fixed.**

Current `1e62683` `ripple_adder8` auto sample 2 (exact single test): end-to-end
39.0770608 s (wrapper 39.1134 s); peak working set 681,500,672 B, peak private
696,287,232 B; leaf/top exhaustive 154/0 ms; leaf/top manifest 328/19,898 ms;
leaf/top `route_nets` 584/17,088 ms; top certify 20,714 ms.

Current `1e62683` `ripple_adder8` auto sample 3 (exact single test): end-to-end
38.1103689 s (wrapper 38.2970 s); peak working set 681,558,016 B, peak private
695,390,208 B; leaf/top manifest 345/18,840 ms; leaf/top exhaustive 154/0 ms;
leaf/top `route_nets` 573/17,192 ms; top certify 19,639 ms.

Current `1e62683` `ripple_adder8` auto median (samples 1-3; sample 1 is the
flagged row above): end-to-end 39.0770608 s, wrapper 39.1134 s; peak working
set median 681,500,672 B (max 681,558,016 B); peak private median
696,287,232 B (max 696,365,056 B). This is +10.3253% versus the retained
baseline auto median (35.4198535 s), exceeding the plan's <=5%
no-regression gate at the same thread setting. **Verdict: FAIL** at parent
revision `1e62683` for the `ripple_adder8` auto case — the three-sample median
confirms a genuine regression, not sampling noise, as thread=1 (-2.6%) and
thread=2 (-2.99%) both stayed within the gate while only auto regressed.

**Root cause (Opus).** The retained baseline computed each manifest sweep's
worker count as `min(requested_workers, transition_count)`. This layer's
shared compile-wide worker-budget coordination (Task 3, `685dfa5`) instead
applied a single small-work serial threshold (8) to every sweep kind,
including manifest; with `ripple_adder8`'s top-level manifest sweep at 68
transitions, that shared threshold cut the auto-requested 12 workers down to 8
instead of `min(12, 68) = 12`, adding synchronization/scheduling overhead
without enough additional parallel work to amortize it, producing the
regression measured above.

**Fix.** Commit `d43e62f` (`perf: restore manifest sweep parallelism`)
restores the manifest sweep's own `min(requested, transitions)` computation
(`MIN_TRANSITIONS_PER_CERTIFICATION_WORKER = 1`, i.e. `min_items=1`) while
leaving the exhaustive sweep's shared small-work threshold at 8 unchanged. 22
targeted tests pass against the fix.

**M2 (final exact-range Opus review): this separate manifest threshold is a
measured, deliberate deviation from the plan's Task 2 step 2 (share one
threshold across exhaustive and manifest sweeps), not an unreviewed drift
from the plan.** The shared-threshold design measurably violated this
layer's own <=5% no-representative-case-regression retention gate (the
`ripple_adder8` auto FAIL above, +10.3253% versus baseline). Between the
plan's specific implementation instruction (one shared threshold) and the
plan's own overriding retention gate (<=5% regression), the retention gate
outranks the implementation instruction: a plan step that produces a
measured gate failure is corrected in favor of the gate, not followed past
the point of failure. `d43e62f` is that correction, and it is recorded here
explicitly as a measured deviation rather than left implicit.

**Debt (I2, from the verified whole-layer Opus review): the `min_items=1`
choice is measured for large sweeps only — now honestly documented in source
as of `0f5a2db`/`f22bf8e`.** `MIN_TRANSITIONS_PER_CERTIFICATION_WORKER = 1`
is measured and accepted as beneficial specifically at the transition counts
this report actually exercised — `ripple_adder8`'s 68-transition manifest
sweep and the other large-circuit cases above — and the aggregate `cargo test
--lib` and complete-verification results show no regression at those and
every other measured case. It is **not** measured for smaller manifest
sweeps: at `min_items=1`, a narrow sweep would be granted up to its full
transition count in workers with no small-work floor at all, and whether that
is a net win or a net loss (thread spin-up/synchronization overhead against a
handful of units of actual work) has not been benchmarked.

**Corrected unmeasured range (per `f22bf8e`): below 68 transitions, not
"2-7".** An earlier draft of this debt note (and `0f5a2db`'s own initial doc
comment) described the unmeasured range as specifically "2-to-7-transition."
Commit `f22bf8e` corrects this: 68 transitions is the smallest case this
report or the source's own tests actually measured, so the honest unmeasured
range is **everything narrower than 68 transitions**, not a specific narrow
band. `f22bf8e`'s commit message states this explicitly: "The constant's
ponytail note said the unmeasured range was 2-7 transitions. 68 is the
smallest case actually measured, so the unmeasured range is everything
narrower than that." This is carried forward as **debt, not as a measured
fact**: a bounded small/mid-manifest crossover benchmark (sweeping transition
counts below 68 at auto) is owed before claiming `min_items=1` is optimal
across the whole sub-68 range, not just beneficial at the 68-transition-and-up
end that regressed under the shared threshold=8 policy. Both
`certification_workers`'s doc comment and the renamed
`worker_policy_scales_each_threshold_independently` test's assert message now
state this measured-cases-only rationale directly in source, closing the gap
where this report's prose was the only place documenting the honest scope.

**Post-fix re-verification at `d43e62f`.** The same `ripple_adder8` auto
three-sample matrix was re-run against the fix:

Current `d43e62f` `ripple_adder8` auto sample 1 (exact single test): end-to-end
36.5828467 s (wrapper 36.9168 s); peak working set 681,934,848 B, peak private
695,095,296 B; leaf/top exhaustive 159/0 ms; leaf/top manifest 273/17,265 ms;
leaf/top `route_nets` 590/17,295 ms; top certify 18,057 ms.

Current `d43e62f` `ripple_adder8` auto sample 2 (exact single test): end-to-end
36.8806159 s (wrapper 37.1163 s); peak working set 682,512,384 B, peak private
697,888,768 B; leaf/top exhaustive 159/0 ms; leaf/top manifest 284/17,558 ms;
leaf/top `route_nets` 575/17,282 ms; top certify 18,368 ms.

Current `d43e62f` `ripple_adder8` auto sample 3 (exact single test): end-to-end
37.0887549 s (wrapper 37.1263 s); peak working set 681,824,256 B, peak private
696,348,672 B; leaf/top manifest 273/17,365 ms; leaf/top exhaustive 159/0 ms;
leaf/top `route_nets` 571/17,705 ms; top certify 18,176 ms.

All three post-fix samples print `manifest_workers=12` at both the leaf and
top manifest sweeps and quality 608 ticks / 70,603 blocks, matching every
other `ripple_adder8` row in this report.

Current `d43e62f` `ripple_adder8` auto median (samples 1-3): end-to-end
36.8806159 s, wrapper 37.1163 s; top manifest median 17,365 ms; peak working
set median 681,934,848 B (max 682,512,384 B); peak private median
696,348,672 B (max 697,888,768 B). This is +4.1241% versus the retained
baseline auto median (35.4198535 s), under the plan's 5% no-regression
threshold (37.1908462 s). **Verdict: PASS.**

Fingerprints are not reported for these six 1e62683/d43e62f samples because
the temporary diagnostic fingerprint capture in `seed.rs` (see Environment
above) had already been reverted before these runs were taken; do not treat
fingerprint identity as observed for these six runs. Case/candidate/world
identity for the auto setting is instead covered by the committed Task 4
1/2/auto acceptance matrix and the complete-verification commands below, not
by this benchmark row.

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

Current `d43e62f` `alu8` auto guard (exact single test, post-fix
re-verification): PASS; end-to-end 122.6107019 s (wrapper 122.7566 s); peak
working set 1,635,373,056 B, peak private 1,676,464,128 B; leaf/alu4/top
exhaustive 1,818/0/0 ms; manifest 445/9,092/54,614 ms. Worker diagnostics:
leaf exhaustive/manifest granted workers = 12/12; alu4-level manifest granted
workers = 12 (up from the pre-fix `1e62683` row's alu4 manifest workers=5,
another direct confirmation of the manifest-threshold fix); top manifest
granted workers = 10 (unchanged, still the 1-GiB memory ceiling for this
case, not the small-work threshold). Quality 972 ticks / 213,833 blocks,
matching every other `alu8` row in this report. End-to-end is 7.88% faster
than the retained-foundation auto figure for this case (133.1050031 s):
(133.1050031 - 122.6107019) / 133.1050031 ≈ 7.884%. **Verdict: PASS**, well
within the plan's 5% no-regression gate — this is an improvement, not a
regression, and the fix made this guard case faster than it already was
pre-fix (127.3211882 s -> 122.6107019 s).

`94e171b` (`test: bind manifest worker policy to production`) is a
behaviour-only test/constant-binding commit on top of `d43e62f` with no
production semantic change, so this `alu8` auto guard result — measured at
`d43e62f` — applies unchanged at `94e171b` and was not independently re-run
for this case.

## Speedup arithmetic

**Provenance note (relates to I4 above): every figure in this block is
pre-fix, `1e62683` arithmetic.** All four lines below use pre-fix `1e62683`
`multiplier4` thread=1/thread=2/auto data (top-only exhaustive denominators
except where marked otherwise). Per the I4 correction above, the t2 lines are
historical and superseded for the retention decision — thread=1 is unaffected
by `d43e62f` and remains valid at `HEAD`, but thread=2 was never re-measured
post-fix. The auto line here is likewise the pre-fix snapshot; the final,
`HEAD`-valid auto ratios (both summed-all-level and, per the I5 correction,
top-only) are in "Final `HEAD` (`94e171b`) `multiplier4` auto re-verification"
above, not here. This block is retained as the historical record of the
pre-fix arithmetic that first established the regression context, not as
`HEAD` evidence.

```text
PHASE exhaustive speedup (t2 vs t1)   = median(t1 exhaustive) / median(t2 exhaustive)   = 1.86x (also 1.93x versus retained baseline serial median)                          [pre-fix 1e62683, historical/superseded]
PHASE exhaustive speedup (auto vs t1) = median(t1 exhaustive) / median(auto exhaustive) = 4.71x (also 4.88x versus retained baseline; top-level-only figures, corrected to use the independently-computed top-exhaustive median 174,208 ms, not the erroneous 174,128 ms figure a prior draft carried over from sample 1's row)   [pre-fix 1e62683; see the HEAD auto section above for current-HEAD ratios]
end-to-end change (t2 vs t1)          = (median(t2 end-to-end) - median(t1 end-to-end)) / median(t1 end-to-end) = -44.00% (594.1709836 s vs 1061.0744303 s); separately, 42.67% faster than the retained baseline thread=2 single sample (1036.3501146 s, not a median)   [pre-fix 1e62683, historical/superseded]
end-to-end change (auto vs t1)        = (median(auto end-to-end) - median(t1 end-to-end)) / median(t1 end-to-end) = -73.65% (279.5675934 s vs 1061.0744303 s); versus retained baseline auto median, end-to-end speedup is 3.50x   [pre-fix 1e62683; HEAD auto end-to-end is 271.5298186 s, see above]
```

At `1e62683`, Gate 1 (this layer) required the first line >= 1.5x, and both
t2 lines above cleared >=1.5x with more than 20% margin, so at that revision
the Opus binding adjudication's escalation clause (additional t2 samples if
the margin over 1.5x is under 20%) was not triggered. This pre-fix result is
not the basis for the current Gate 1 decision at `HEAD`, which rests on the
`HEAD` auto matrix and valid `HEAD`/thread=1 comparisons instead (see I4
above); no `HEAD` thread=2 samples are being requested or invented here.

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
11,661,312 B against the 10-extra-worker model allowance of 966,582,720 B.

**Correction (I6, from the verified whole-layer Opus review).**
`implied_headroom ≈ 0.0483` must be read as an **inference/lower-bound
artifact derived from whole-process peak-working-set samples**, not as a
direct per-worker measurement. `PeakWorkingSet64` is an OS-level, whole-process
metric sampled externally by the PowerShell wrapper (see "Measurement
method" above); it cannot attribute memory to any individual worker thread,
so `implied_headroom` is backed out algebraically from the *difference*
between two whole-process peaks under an assumed linear `(N-1) *
per_worker_bytes` model — it is consistent with, but does not directly
measure, per-worker memory. Because the actual per-worker contribution could
be smaller than this inferred figure (e.g. if peak working set is dominated by
process-wide allocations shared across workers rather than N independent
per-worker copies), 0.0483 should be treated as a **lower-bound-style
inference**, not a validated upper bound on true per-worker headroom. The
pre-existing measurement debt from Task 3 — that the `World` side-index
memory must be measured and folded into `WORLD_COPY_HEADROOM` before claiming
a bounded peak — remains open and is **not** closed by this increment check;
this check is additional corroborating evidence, not a replacement for that
direct measurement. Given both caveats, the byte ceiling appears adequate on
the evidence gathered so far, but "the byte ceiling is adequate" should not be
overstated as a fully validated per-worker bound. Separately: host-core
independence (memory not growing with host core count) is a **code-level
property of `sweep_worker_budget`'s auto-clamp-to-`available_parallelism`
design**, not something this single-host run can empirically prove across
different core counts — it follows from reading the worker-budget code, not
from this report's measurements — whereas the `REDA_CERT_MEMORY_BYTES=1`
memory-budget cap enforcement fixture below **is** direct runtime evidence
(it shows `granted` actually collapsing to 1 under a starved byte budget on
this host), and the two should not be conflated as the same kind of evidence.
No double
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
exhaustive not yet routed through it), at `0f5a2db` (full suite, see
"authoritative `HEAD`" note below), and focused at `f22bf8e` (the current
authoritative `HEAD`):

```powershell
cargo test --lib
```

| State | Wall time |
|---|---:|
| Before this layer (`f899d6c`) | compile 47.97 s, libtest 694.53 s, wrapper 743.0772 s (913 passed / 0 failed / 71 ignored) |
| Full suite at `0f5a2db` (production-equivalent, pre-`f22bf8e`) | libtest 735.48 s, wrapper 735.6283 s (928 passed / 0 failed / 74 ignored) |
| Focused certification module at `f22bf8e` (current authoritative `HEAD`) | libtest 0.50 s, wrapper 11.2198 s including a 10.12 s compile (23 passed / 0 failed / 979 filtered out) |

**Authoritative `HEAD` note.** The layer's current authoritative `HEAD` is
`f22bf8e`, a test/comment-only review-fix commit on top of `0f5a2db`, with
"No production semantics change" (per its own commit message) and no
change outside `src/compile/fragment_synth/certification.rs`. The full
`cargo test --lib` run above was taken at `0f5a2db`, one commit before
`f22bf8e`; it is cited as **production-equivalent** evidence for `f22bf8e`
because the diff between the two revisions is limited to doc
comments/assert-message wording and one test's already-passing assertions
(pinning worker counts), not to any production code path. The
`f22bf8e`-specific row above is a **focused, current** rerun scoped to
exactly the module `f22bf8e` touched
(`cargo test --lib compile::fragment_synth::certification::tests --
--nocapture`), confirming the corrected/added assertions actually execute
and pass against the real `f22bf8e` source, rather than only against
`0f5a2db`. A fresh full `cargo test --lib` re-run at `f22bf8e` itself has not
been supplied to this report and is not claimed; do not read the full-suite
row above as having run against `f22bf8e`'s exact tree.

Note the test counts differ across rows (913 passed / 71 ignored at
`f899d6c` vs. 928 passed / 74 ignored at `0f5a2db`): this is expected, not a
discrepancy to explain away — this layer's own commits add new tests (Task
4's thread-count-matrix tests, the `94e171b`/`0f5a2db` policy-constant and
`FunctionalMismatch` tests, and other `#[cfg(test)]` additions along the
way), so the lib-test binary at `0f5a2db` legitimately contains more tests
than it did at `f899d6c`. The wall-time comparison above is still a
like-for-like `cargo test --lib` invocation on each revision's own full test
suite; it is the *test suite contents*, not the invocation, that differ
across rows.

This is an **aggregate, confounded** comparison, not an isolated lock cost:
`a6588b9` already routes exhaustive through the same guarded shared helper
that carries the process-wide sweep lock, so the before/after delta reflects
the combined effect of exhaustive parallelism, the shared lock now covering
exhaustive as well as manifest, and Task 3's worker-budget coordination
together. It shows whether independent-test-compile serialization became
visible at the `cargo test --lib` scale, not the lock's cost in isolation.

**Lock-after diagnostic attempt (failed, not final evidence).** At `HEAD`
(`94e171b`), `cargo test --lib` was run once with compile already warm: test
result 926 passed / 1 failed / 74 ignored, finished in 733.82 s; wrapper
`BENCH_LOCK_AFTER elapsed_s=733.9816 exit=101`. The sole failure was
`certification_sweeps_serialize_and_recover_after_poison`, panicking on a
`recv_timeout(1s)` `Timeout` (line references to the pre-fix source at the
time of this diagnostic are stale after `b9e0c63` restructured this test's
body and are not repeated here; see `certification.rs:1548-1580` at current
`HEAD` for the post-fix test, checked against current source, which no
longer contains that `recv_timeout(1s)` call at all); the poisoned worker's
`SendError` was downstream of that timeout, not a separate defect. An
immediate exact isolated rerun of just that test passed
1/1 in 0.03 s, with the test's intentional poison panic caught as expected —
consistent with a timing-sensitive flake under whole-suite contention (many
concurrent tests competing for the shared sweep lock and CPU) rather than a
reproducible logic defect. This single failed full run is recorded as
diagnostic history only; it was superseded by the clean full rerun below and
was never itself treated as the lock-after measurement.

**Lock-after clean result at `b9e0c63` (superseded by the `0f5a2db` full-suite
row above, kept as the intermediate record).** `cargo test --lib` passed at
`b9e0c63`: 927 passed / 0 failed / 74 ignored / 0 measured / 0 filtered out;
libtest 735.54 s; wrapper `BENCH_LOCK_AFTER revision=b9e0c63
elapsed_s=735.6832 exit=0`. This was the "after this layer" figure before
`0f5a2db` added its `FunctionalMismatch` fixture and renamed test (which grew
the passed count from 927 to 928); it is retained here as the record of the
lock-flake fix's own clean re-verification, but the table above now cites the
`0f5a2db` full-suite result (928 passed / 0 failed / 74 ignored, libtest
735.48 s, wrapper 735.6283 s) as the current production-equivalent figure.
Compared to the lock-before wrapper time at `f899d6c`
(743.0772 s), both the `b9e0c63` and `0f5a2db` wrapper times are about 0.99%
faster ((743.0772 - 735.6832) / 743.0772 ≈ 0.995%; (743.0772 - 735.6283) /
743.0772 ≈ 1.003%) — no aggregate `cargo test --lib`
regression at either revision. As stated above, this comparison remains an **aggregate,
confounded** measurement (exhaustive parallelism, the shared sweep lock now
covering exhaustive as well as manifest, and Task 3's worker-budget
coordination all changed together between these two revisions), not an
isolated measurement of the lock's own cost.

**Correction:** the diagnostic run itself made no edits, but it directly led
to a committed test-only fix, `b9e0c63` (`test: drop wall-clock deadline from
sweep lock handoff`), so the claim that "no source or test file was modified
in response to this diagnostic" no longer holds and is corrected here.
`b9e0c63` replaces the flaky test's positive-path `recv_timeout(1s)` with a
blocking `recv` taken only after `drop(held)` releases the lock guard, so the
test no longer races a wall-clock deadline against scheduler contention on a
busy machine. The test's 20 ms *negative* mutual-exclusion check (proving two
holders cannot run concurrently) and its poison-recovery assertion are both
retained unchanged. Production code is unchanged by this commit — this is a
test-only fix. The prior bounded-`recv_timeout` construction was already
unsound in the same way a `drop`-then-blocking-`recv` is: if the lock were
ever leaked permanently (never released), the old scope-join would also hang
indefinitely waiting for the timeout thread's own join, so the fix does not
introduce a new class of hang — it removes a false "bounded handoff" contract
the timeout appeared to promise but could not actually guarantee under
contention.

Focused re-verification after the fix: `cargo test --lib
certification -- --nocapture` scoped to the certification module reported 22
passed / 0 failed / 979 filtered out, 0.50 s.

**Independent Opus review (`b9e0c63`).** SPEC: PASS. CODE QUALITY: PASS. No
Critical findings. Verdict: retain `b9e0c63` as-is. The Important finding
that flagged this report's now-corrected contradiction (claiming no test file
was touched, immediately followed by a test-only fix commit) is addressed by
this edit. A second review note — that a thread can still wait indefinitely
if the lock is leaked and never released — is accepted and documented above
rather than acted on with a code change, since it describes a pre-existing,
unbounded-wait property shared with the removed `recv_timeout` construction,
not a regression introduced by this fix. Three Minor findings were deferred,
none blocking retention: the pre-existing 20 ms negative mutual-exclusion
check can in principle pass vacuously if the second thread never gets
scheduled in time; the latent sender could in principle panic a second time
if the main thread were unwinding when it sends, a case that does not arise
in this test's actual execution order; and the unrelated 5 s indexed-chunk
deadline elsewhere in the suite was reviewed and judged justified, not a
copy of this same defect.

**Commit `0f5a2db` (`test: cover exhaustive functional mismatch and correct
worker-policy claims`) — resolves I1, I2 and I3 from the Round 4 whole-layer
review below.** This test/comment-only commit to
`src/compile/fragment_synth/certification.rs` (73 insertions / 7 deletions,
one file):
- **I1 resolved:** renames `manifest_sweeps_open_one_worker_per_heavy_transition`
  to `worker_policy_scales_each_threshold_independently` and corrects its
  comment to state plainly that the test calls `certification_workers`
  directly, pins the policy each threshold produces, and does **not** prove
  callsite wiring — replacing the prior overstated "binds production" framing
  with an honest description, rather than adding an abstraction to make the
  old claim retroactively true.
- **I2 addressed (partially; see `f22bf8e` below for the full fix):**
  `MIN_TRANSITIONS_PER_CERTIFICATION_WORKER`'s doc comment no longer states an
  unmeasured inference as fact; the value and behaviour are unchanged, and the
  stated rationale is now the measured large-sweep evidence
  (`ripple_adder8`'s 68-transition top manifest sweep, plus the aggregate
  `cargo test` results above showing no regression), with an explicit
  ponytail note that flags the small-sweep crossover as unmeasured.
- **I3 resolved:** adds the plan-required real `FunctionalMismatch` refusal
  fixture,
  `exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count`.
  A realised NOT-gate world (`not_netlist_with_inputs(5)`) is certified
  against a same-I/O BUF spec built from two NORs, so every one of the 32
  vectors disagrees and the refusal is a real `check_outputs` disagreement,
  not a completion-order/event-cap substitute. At 1, 2 and 4 workers the
  refusal must be `FunctionalMismatch` at `manifest_index` 0, output `"y"`,
  expected `false`, actual `true`. Mutation-validated: swapping the
  expected/actual fields, or reversing the ordered reduction, each make the
  test fail.

Independent Opus review of `0f5a2db`: **SPEC: PASS. CODE QUALITY: APPROVED.**
Verdict: retain. The review verified the fixture actually exercises the real
`check_outputs` path (not a mock) and that the reported refusal mask is `0`
(manifest index 0), matching the fixture's design. Its remaining
Important/Minor findings were documentation/worker-count-assertion gaps,
addressed by `f22bf8e` below (not by any further source change beyond
`f22bf8e` itself).

**Commit `f22bf8e` (`docs: bound the manifest threshold claim to measured
cases; pin test worker counts`) — the current authoritative `HEAD`, fully
resolves I2.** Test/comment-only commit to the same file (21 insertions / 9
deletions):
- Both the `certification_workers` doc comment and the worker-policy test's
  assert message no longer describe the manifest threshold as inherently
  justified per item; both now say one-per-worker measured better
  specifically on the wide manifest sweeps this report exercised, and that
  narrower sweeps are unmeasured.
- Corrects the unmeasured-range claim: the prior wording said "the
  2-to-7-transition crossover is unmeasured"; 68 transitions
  (`ripple_adder8`'s top manifest sweep) is the smallest case this report
  actually measured, so the honest unmeasured range is **everything narrower
  than 68 transitions**, not specifically "2-7". This corrects the I2 debt
  note earlier in this report (see the updated I2 text above, which should
  now be read as "below 68 transitions," not "2-7 transitions").
- `exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count`
  now additionally asserts that each requested 1/2/4 worker setting actually
  resolves to that many granted workers over the fixture's 32 vectors under
  `MIN_VECTORS_PER_CERTIFICATION_WORKER` — closing the gap where raising that
  threshold could silently collapse the parallel cases to one worker and
  leave the ordering coverage serial while the test stayed green.
- Commit message states "No production semantics change."

Controller focused re-verification at `f22bf8e`: `cargo test --lib
compile::fragment_synth::certification::tests -- --nocapture` reported 23
passed / 0 failed / 979 filtered out; libtest 0.50 s, wrapper 11.2198 s
(including a 10.12 s compile). This is a growth from the `0f5a2db`-era 22
passed (see the superseded "Focused re-verification after the fix" figure
above) to 23 passed, consistent with `f22bf8e` adding assertions to an
existing test rather than adding a new one.

The scoped, independent Opus review of `f22bf8e` has now completed: **SPEC
PASS, CODE QUALITY PASS, RETAIN as-is**, no Critical or Important findings.
See "Opus review" below for the full verdict and its three nonblocking
deferred Minor findings.

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
| `cargo test --lib` | PASS, current production-equivalent evidence at `0f5a2db` (one commit before the current authoritative `HEAD` `f22bf8e`, test/comment-only diff, no production semantics change): 928 passed / 0 failed / 74 ignored (`b9e0c63` — 927 passed / 0 failed / 74 ignored, and before that an earlier attempt at prior `HEAD` `94e171b` had failed with 926 passed / 1 failed / 74 ignored, see "Lock-after diagnostic attempt" above — is retained only as historical record, superseded by the `0f5a2db` figure) | libtest 735.48 s / wrapper 735.6283 s (`0f5a2db`; `b9e0c63` historical: libtest 735.54 s / wrapper 735.6832 s) |
| `every_extra_circuit` (release, ignored) | PASS at `HEAD` (`b9e0c63`): 1 passed / 0 failed / 1000 filtered out; all 16 named circuits printed OK | test 48.67 s (release compilation beforehand took 1m17s, not part of the test-time figure) |
| `every_large_circuit` (release, ignored) | PASS at `HEAD` (`b9e0c63`): 1 passed / 0 failed / 1000 filtered out; all 4 named circuits printed OK | test 465.66 s |
| `fragment_synth_acceptance` (release) | PASS at `HEAD` (`b9e0c63`): 4 passed / 0 failed / 0 ignored / 0 filtered out | test 0.68 s (release target build beforehand 45.20 s, excluded) |
| `fragment_acceptance` binary run | PASS at `HEAD` (`b9e0c63`), rerun directly (not cited from Task 4) — see detail below | exit=0, elapsed 1407.0323 s |
| pinned seven-segment IO contract | PASS at `HEAD` (`b9e0c63`), rerun directly (not cited from Task 4) — see detail below | test 403.26 s, wrapper 413.8875 s (includes 10.14 s debug build) |
| `cargo clippy --lib --tests` | PASS, current at `f22bf8e` (the authoritative `HEAD`), exit=0 — see detail below (`b9e0c63` retained below as historical) | cargo 7.20 s / wrapper 7.3466 s (`f22bf8e`); 13.8244 s (`b9e0c63`, historical) |
| `git diff --check` | PASS at `HEAD` (`b9e0c63`), exit=0, no output; `git status` shows only the intended untracked report file, tracked source clean | — |

**`every_extra_circuit` detail.** Run as the exact test at `HEAD` (`b9e0c63`):
`cargo test --release --lib
compile::fragment_synth::seed::tests::extra_circuits::every_extra_circuit_certifies_with_the_topology_aware_seed
-- --exact --ignored --nocapture --test-threads=1`. Result: 1 passed / 0
failed / 1000 filtered out, test finished in 48.67 s (the preceding release
compilation took 1m17s and is excluded from that test-time figure). All 16
named circuits printed OK: `segment_b`, `segment_c`, `segment_d`, `segment_e`,
`segment_f`, `segment_g`, `majority3`, `parity4`, `decoder_2_to_4`,
`mux_4_to_1`, `and8`, `or8`, `equal4`, `incrementer4`, `ripple_adder2`,
`ripple_adder4`.

**`every_large_circuit` detail.** Run as the exact test at `HEAD` (`b9e0c63`):
`cargo test --release --lib
compile::fragment_synth::seed::tests::extra_circuits::every_large_circuit_certifies_with_the_topology_aware_seed
-- --exact --ignored --nocapture --test-threads=1`. Result: 1 passed / 0
failed / 1000 filtered out, test finished in 465.66 s. Four named circuits
printed OK: `ripple_adder8` (gates=168, ticks=474, blocks=100,615,
87.4048092 s), `alu4` (gates=172, ticks=348, blocks=72,533, 50.9591926 s),
`alu4_full` (gates=246, ticks=588, blocks=146,590, 160.3958715 s), and
`multiplier4` (gates=228, ticks=591, blocks=93,254, 166.8888313 s); these four
per-case times sum to 465.6487046 s, matching the reported total within
rounding.

**`fragment_synth_acceptance` detail.** Run at `HEAD` (`b9e0c63`): `cargo test
--release --test fragment_synth_acceptance -- --nocapture`. Result: 4 passed
/ 0 failed / 0 ignored / 0 filtered out, test time 0.68 s (the preceding
release target build took 45.20 s and is excluded from that test-time
figure). Passed tests: `shuffled_budget_orders_are_seeded_repeatable_and_order_sensitive`,
`checked_failure_report_names_every_failed_condition_and_no_shipping_source_exists`,
`replacement_corpus_is_complete_and_has_checked_pinned_glyph_io`,
`a_failed_gate_cannot_generate_a_shipping_configuration`.

**`fragment_acceptance` binary run detail.** This command was rerun directly
at `HEAD` (`b9e0c63`) rather than cited from Task 4, because `d43e62f` is a
production change committed after `685dfa5` and the original citation
condition (production unchanged since `685dfa5`) no longer held:

```powershell
$parallelCertDir = "C:\Users\LTY\AppData\Local\Temp\reda-parallel-cert-c0ae5b3f-7c74-4828-9025-292f93846037"
New-Item -ItemType Directory -Path $parallelCertDir | Out-Null
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output "$parallelCertDir/acceptance.json" --shipping-source "$parallelCertDir/shipping_config.rs" --shuffle-seed 0x5245444120260831
```

Result: PASS, exit=0, elapsed 1407.0323 s. Program result:
`replacement_gate_passed=false`, `shipping_evaluations=None`, `failures=30`.
`acceptance.json` exists at the temp output path, size 173,132 bytes,
SHA-256 `006429A36BD9169B316B7A576E0667EE74F313461F4607992AC1D3360E1C9A8B` —
byte-identical to the Task 4 authority hash cited below.
`shipping_config.rs` does not exist at the temp output path, which is
expected because the replacement gate is `false` (a failed gate must not
produce a shipping configuration, per
`a_failed_gate_cannot_generate_a_shipping_configuration` above).

**Pinned seven-segment IO contract detail.** Like the `fragment_acceptance`
binary run above, this command was rerun directly at `HEAD` (`b9e0c63`)
rather than cited from Task 4, because `d43e62f` is a production change
committed after `685dfa5` and the original citation condition no longer held:

```powershell
cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --nocapture
```

Result: PASS, 1 passed / 0 failed / 0 ignored / 6 filtered out. Test time
403.26 s; wrapper 413.8875 s (includes a 10.14 s debug build beforehand, not
counted as test time). This current-`HEAD` rerun proves the checked
seven-segment 11-pin contract still holds after `d43e62f`.

**Corpus-level 1/2/auto matrix provenance (superseded, not same-revision
proof).** The long 1/2/auto thread-count correctness matrix (`cargo test
--release --lib certification_thread_counts -- --ignored --nocapture
--test-threads=1`: 3 passed, 0 failed, 997 filtered out, 12,331.98 s; see
`.superpowers/sdd/2026-09-07-portable-parallel-certification/task-4-report.md`
"Controller acceptance evidence") was run at parent revision `1e62683`, not at
the current authoritative `HEAD` (`f22bf8e`). It is cited here as superseded,
corpus-level evidence from a prior revision, not as same-revision proof that
the full 1/2/auto correctness property still holds bit-for-bit at `HEAD` — do
not read the 12,331.98 s figure as having run against
`d43e62f`/`94e171b`/`b9e0c63`/`0f5a2db`/`f22bf8e`.
Combined with same-revision evidence at `HEAD` — the parallel/serial
equivalence unit tests in `certification.rs` (which do run at `HEAD` and
exercise the actual worker-count code paths), the current standalone
`fragment_acceptance` binary's SHA-256 match to the Task 4 authority hash
above, and this section's current pinned seven-segment IO rerun — the residual
risk that `HEAD`'s behaviour has silently diverged from the Task 4 corpus
matrix is reduced, but not eliminated by direct measurement; a fresh full
1/2/auto corpus rerun at `HEAD` remains outside this report's scope and is not
claimed to have happened. This report's own samples above target the
PHASE-level speedup gate that Task 4 did not measure (Task 4 explicitly adds
no timing assertion).

**Gap (I3, from the verified whole-layer Opus review) — resolved by commit
`0f5a2db`, further hardened by `f22bf8e`.** The Task 4 corpus matrix cited
above was supposed to include a fixture that exercises the
`FunctionalMismatch` refusal path, but that fixture was missing; Task 4's
matrix silently substituted an event-cap refusal case in its place instead,
so the `FunctionalMismatch` refusal path itself was not actually exercised by
the cited 12,331.98 s matrix (or by anything else in this report at the time
this gap was first flagged). **This is now resolved:** commit `0f5a2db` adds
the real fixture,
`exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count`
(see the "Commit `0f5a2db`" detail above) — a realised NOT-gate world
certified against a same-I/O two-NOR BUF spec, so every one of 32 vectors
genuinely disagrees via the real `check_outputs` path (not a mock), with the
refusal asserted as `FunctionalMismatch` at `manifest_index` 0 across 1, 2 and
4 workers, and mutation-validated. Commit `f22bf8e` further pins that each
requested worker setting (1/2/4) actually resolves to that many granted
workers over the fixture, closing the residual risk that a future threshold
change could silently collapse the parallel cases to one worker. Controller
result at `f22bf8e`: focused `cargo test --lib
compile::fragment_synth::certification::tests` 23 passed / 0 failed / 979
filtered out (see the "Commit `f22bf8e`" detail above). The corpus-matrix
provenance discussion above should now be read as: the `FunctionalMismatch`
refusal path is covered by this dedicated fixture at the current authoritative
`HEAD` (`f22bf8e`), independent of and in addition to the still-`1e62683`-provenance
12,331.98 s corpus matrix.

**`cargo clippy --lib --tests` detail.** Run at the current authoritative
`HEAD` (`f22bf8e`): PASS, exit=0, cargo 7.20 s / wrapper 7.3466 s. Clippy
emitted existing warnings — 27 in the `lib` target and 31 in the lib-test
target (counts include duplicates across targets) — but no errors. These are
pre-existing warnings, not introduced by this layer; this run is **not** a
warning-free clippy pass and is not reported as one. (Earlier run at
`b9e0c63`, retained as historical: PASS, exit=0, elapsed 13.8244 s, same
warning counts, no errors.)

**`git diff --check` detail.** Run at `HEAD` (`b9e0c63`): PASS, exit=0, no
output. `git status` at the time of this check showed only the intended
untracked report file (`docs/superpowers/reports/2026-09-07-portable-parallel-certification.md`);
tracked source is clean.

With this, every row of the "Complete verification" table above is measured.
The whole-layer Opus review has run (see "Opus review" below, Round 4), its
two gating findings (I1, I3) landed as commits `0f5a2db`/`f22bf8e` and were
confirmed by `f22bf8e`'s own scoped review (Round 5: SPEC PASS, CODE QUALITY
PASS, RETAIN as-is), and the final exact-range review across
`61ffdb1..f22bf8e` (Round 6) has also completed: SPEC PASS, CODE QUALITY
PASS, KEEP, no Critical or Important findings — see "Opus review" and
"Retention decision" below. The retention decision is **KEEP, final**, with
no Opus review item outstanding.

## Cross-machine scope

This host (12 logical processors, 68,664,922,112 bytes physical memory)
establishes the layer's retention gate. Per the plan and spec, this report
must not claim cross-machine scaling until the same release matrix has been
run and recorded on a second, distinct machine.

## Incident log

**Working-tree incident (resolved).** An earlier attempt at this report/ledger
update by another Opus session ran a whole-repository `cargo fmt` and used
`git stash push -u` to set aside the then-untracked report file, then was
interrupted before committing. The source tree was reset back to its
pre-`fmt` state via that same stash entry, and this report file was recovered
from the exact stash blob rather than reconstructed, so no report content was
lost. `git status`/`git diff` after recovery showed no unintended tracked-file
diff remaining (the `cargo fmt` change did not survive the reset). Per
worktree stash-safety practice, the stash entry itself is intentionally left
in place; its cleanup is deferred until this report is committed, so the
recovery path stays auditable until then.

## Opus review

Whole-layer Opus 5 review, covering the full `f899d6c`..`b9e0c63` range
against every required explicit finding (ordered error/panic reduction, cap
accounting, thread-budget restoration, nested oversubscription, memory, pinned
IO, benchmark arithmetic), is recorded below as Round 4 and has run; see
that round for its findings and verdict. Rounds 1-3 below are the earlier
commit-scoped reviews of only the `ripple_adder8`-regression fix commits and
the lock-after test flake fix, not the whole layer; they are retained for
history and do not substitute for Round 4. Round 4's two gating findings
(I1, I3) have since been resolved by commits `0f5a2db`/`f22bf8e`, and
`f22bf8e`'s own scoped independent review is recorded below as Round 5 and
has also now completed (**SPEC PASS, CODE QUALITY PASS, RETAIN as-is**).
Round 4 was scoped to `f899d6c`..`b9e0c63` before the range was extended, so
it did not by itself cover `0f5a2db`/`f22bf8e`, and Round 5 covered
`f22bf8e` individually rather than the full range; the final, independent
exact-range review across the full `61ffdb1..f22bf8e` range is recorded below
as **Round 6 and has now completed: SPEC PASS, CODE QUALITY PASS, KEEP, no
Critical or Important findings.** No Opus review item remains outstanding.

**Round 1 (commit `d43e62f`).** Opus review found the behavioural diff itself
correct — restoring the manifest sweep's own `min(requested, transitions)`
computation — but did not approve the commit for retention: the accompanying
benchmark evidence was incomplete at that point (the `ripple_adder8` auto
three-sample median and the post-fix `multiplier4`/`alu8` re-verification were
not yet in hand), and the fix was not yet bound into a committed test, so
nothing in the test suite would catch a future regression back to the shared
small-work threshold being applied to manifest sweeps.

**Round 2 (commit `94e171b`, re-review).** Opus re-review of `test: bind
manifest worker policy to production` returned **SPEC/CODE QUALITY:
APPROVED**, with no Critical findings, subject to the correction below.

**Correction (I1, from the verified whole-layer Opus review) — resolved by
commit `0f5a2db`.** This report previously overstated what `94e171b` actually
covers: it does **not** bind the manifest-sweep callsite (the code path that
decides worker counts at the site where the manifest sweep runs) against a
future regression. The test `94e171b` adds directly exercises the
`certification_workers` policy constants only — it is a pure constant/policy-
value test. It would not catch a future change that swapped which constant a
callsite reads, or otherwise changed the callsite's wiring, while leaving the
constants themselves unchanged. This is exactly what the previously-deferred
Minor finding (a test comment overstating the coverage the test actually
asserts) was pointing at; it was elevated from Minor to Important because
"the missing test binding" language previously in this report materially
overstated the regression protection this commit provides. **This is now
resolved:** commit `0f5a2db` renames the test to
`worker_policy_scales_each_threshold_independently` and rewrites its comment
to state plainly that it pins the policy each threshold produces and does not
prove callsite wiring — see the "Commit `0f5a2db`" detail above. `94e171b`'s
test coverage should be read as "the policy constants have these values,"
which is now exactly what the renamed test's own comment says, not an
overstated claim.

**Round 3 (commit `b9e0c63`).** Independent Opus review of the lock-after
test-flake fix: **SPEC: PASS. CODE QUALITY: PASS.** No Critical findings.
Verdict: retain `b9e0c63` as-is. The Important finding raised against this
report — that it claimed no test file was modified in response to the
lock-after diagnostic immediately before a test-only fix commit landed — is
addressed by the correction in the "`cargo test --lib` wall time" section
above. A second review note, that a thread can still wait indefinitely if the
sweep lock is leaked and never released, is accepted and documented rather
than acted on with a code change, since that unbounded-wait property predates
this fix and is unchanged by it. Three Minor findings were deferred, none
blocking retention: the test's pre-existing 20 ms negative mutual-exclusion
check could in principle pass vacuously if the second thread is not scheduled
in time; the latent sender could in principle panic a second time only if the
main thread were already unwinding when it sends, which does not occur in
this test's actual execution order; and an unrelated 5 s deadline elsewhere in
the suite (indexed-chunk handling) was reviewed and judged justified on its
own terms, not an instance of the same defect.

**Round 4 (verified whole-layer review, at the time scoped to
`f899d6c`..`b9e0c63`).** This round reviewed that range and this report's own
claims against the evidence, and found one Critical/report-accuracy item and
six Important items, all addressed by corrections applied directly in the
relevant sections above (cross-referenced by label so they are not
duplicated here). Its findings and this report's corrections remain valid
after the layer range was subsequently extended to `f22bf8e` (see the
"authoritative `HEAD`" updates throughout this report and the note below this
list):
- **C1** (Environment / "Complete verification"): removed stale "may cite
  Task 4" authorizations for the standalone `fragment_acceptance` binary run
  and the pinned seven-segment IO contract — both are recorded as fresh
  reruns at their then-`HEAD` (`b9e0c63`; still production-equivalent at the
  current authoritative `HEAD` `f22bf8e`, since `0f5a2db`/`f22bf8e` touch only
  `certification.rs` test/comment code, not these commands' code paths); the
  long 1/2/auto corpus matrix is relabeled superseded, corpus-level evidence
  from `1e62683`, not same-revision proof.
- **I1** ("Opus review", Round 2 correction) — **resolved by `0f5a2db`**:
  `94e171b` does not bind the manifest-threshold callsite; it exercises the
  policy constants only. `0f5a2db` renames the test to
  `worker_policy_scales_each_threshold_independently` and rewrites its
  comment to state this honestly.
- **I2** ("Raw samples — guard fixtures", Fix/Debt note) — **doc wording
  corrected by `0f5a2db`/`f22bf8e`, benchmark debt still open**:
  `MIN_TRANSITIONS_PER_CERTIFICATION_WORKER = 1` is measured beneficial only
  at the transition counts actually exercised (68-transition and larger); the
  unmeasured range is corrected from a previously-stated "2-7 transitions" to
  the accurate "everything narrower than 68 transitions" (per `f22bf8e`); a
  bounded below-68-transition small-manifest crossover benchmark is still
  carried as debt, not measured fact — only the documentation of that debt
  moved from this report into source comments/assert messages.
- **I3** ("Complete verification", pinned IO section) — **resolved by
  `0f5a2db`, worker-count coverage hardened by `f22bf8e`**: the Task 4 corpus
  matrix's `FunctionalMismatch` refusal fixture was missing and silently
  substituted by an event-cap case; `0f5a2db` adds the real fixture
  (`exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count`,
  mutation-validated, real `check_outputs` path) and `f22bf8e` pins its
  requested-vs-granted worker counts. Controller confirmed 23/23 passed at
  `f22bf8e`.
- **I4** (`multiplier4` thread=2 median): relabeled as historical, pre-fix
  (`1e62683`) evidence, superseded for the retention decision; Gate 1 at
  `HEAD` is decided by the `HEAD` auto matrix and valid `HEAD`/thread=1
  comparisons, with no invented `HEAD` thread=2 number.
- **I5** ("Final `HEAD` (`94e171b`) `multiplier4` auto re-verification"):
  top-only and summed-all-level `PHASE exhaustive` denominators are now
  explicitly labeled everywhere they appear, with consistent `HEAD` top-only
  ratios added (≈4.57x vs current thread=1, ≈4.73x vs retained baseline)
  alongside the retained summed-all-level ratios (≈4.57x, ≈4.74x).
- **I6** ("Memory bound check"): `implied_headroom ≈ 0.0483` is relabeled an
  inference/lower-bound artifact from whole-process peak-working-set
  samples, not a per-worker measurement; the `World` side-index /
  `WORLD_COPY_HEADROOM` measurement debt from Task 3 remains open; host-core
  independence is distinguished as a code-level property of
  `sweep_worker_budget`, separate from the `REDA_CERT_MEMORY_BYTES=1` fixture's
  direct runtime evidence.

Additional prose-only corrections applied where cheap, with no production
cleanup scope added: the temporary `WORK sweep_budget`/fingerprint
instrumentation is now described in the past tense as reverted (confirmed by
the clean `git diff --check` and `cargo clippy` results) rather than as an
outstanding to-do; the drifted `seed.rs` line references in "Measurement
method" were checked against current `HEAD` source and corrected; the
`certification.rs` line references in the lock-after diagnostic (which were
accurate only against the pre-`b9e0c63` source) are marked stale rather than
repeated as if still current; the differing lock-before/lock-after test
counts (913/71 vs 927/74) are explained as expected given this layer's own
test additions, not a discrepancy; and the "Retention decision" section now
states explicitly that `94e171b` travels with `d43e62f` in revert logic
rather than being kept independently.

This round's overall verdict, **as it stood at the time of Round 4**: the
layer's benchmark evidence and gate arithmetic were sound once the above were
corrected, but the final retention verdict remained pending because I1
(source comment/name fix) and I3 (`FunctionalMismatch` fixture test) were
both still in progress separately from this report at that point, with
retention withheld until both landed as commits and a fresh reviewer
confirmed them. **This is now historical, not current:** both landed
(`0f5a2db`/`f22bf8e`), and `f22bf8e`'s own scoped review has since confirmed
them — see Round 5 below.

**Round 5 (independent scoped review of `f22bf8e`).** **SPEC: PASS. CODE
QUALITY: PASS. Verdict: RETAIN as-is.** No Critical or Important findings.
This review confirmed all three of Round 4's I1-I3 remedies as implemented
correctly, confirmed `f22bf8e` introduces no production behavior change,
confirmed the new `FunctionalMismatch` fixture's 32-vector arithmetic and
type inference are sound, and confirmed the 23/23 controller test evidence
above. Three nonblocking Minor findings were deferred, recorded tersely per
the review's own framing, none blocking retention and none reopening code:
- The aggregate `cargo test --lib` no-regression sentence(s) elsewhere in
  this report should be read as **confounded/non-isolating** evidence (they
  bundle multiple concurrent changes), not as an isolated measurement of any
  single commit's cost.
- Sibling worker-count tests outside the `f22bf8e` diff carry an **analogous
  silent-serial risk** (a threshold could collapse a parallel case to one
  worker while the test stays green) that `f22bf8e` did not itself audit or
  fix — noted as an out-of-scope observation, not a defect in `f22bf8e`.
- The new fixture's `state_count` is derived from the netlist's **input**
  count, though the `buffered` netlist is equal by construction (same
  inputs/outputs as the realised netlist) — noted as a minor precision point
  about which structure's input count is being read, not a correctness gap.

**Round 6 (final, independent exact-range review, `61ffdb1..f22bf8e`) —
COMPLETE.** **SPEC: PASS. CODE QUALITY: PASS. Verdict: KEEP.** No Critical or
Important findings. This review covered the full retained layer range in one
pass — the range Round 4 could not, since Round 4 was scoped to
`f899d6c`..`b9e0c63` before the range was extended — and explicitly checked:
ordered error/panic reduction; exact first failure; cap accounting;
unwind-safe restoration; process lock/oversubscription; memory
ceiling/caveats; 1/2/auto identity; pinned IO/whole-world certification;
benchmark arithmetic; the <=5% no-representative-case-regression guard; the
>=1.5x `PHASE exhaustive` gate; `FunctionalMismatch` coverage; and `f22bf8e`'s
closure of Round 4's I1-I3 findings. This is the **final** review item for
this report; no further Opus review is outstanding.

Three additional source/cosmetic minors were deferred as nonblocking
observations, recorded tersely in the same style as Round 5's minors above,
none reopening code:
- **M4:** the spawned-thread TLS explicit-override documentation relies on
  the `leaf_budget = 1` invariant holding; if that invariant is ever relaxed,
  the doc's reasoning would need re-checking alongside it.
- **M5:** an import-order ordering in one file is a cosmetic `rustfmt`
  deviation, not a behavioural issue.
- **M7:** the measured debts already flagged open elsewhere in this report
  (I2's below-68-transition small-manifest crossover benchmark; the Task 3
  `World` side-index/`WORLD_COPY_HEADROOM` measurement debt from I6) remain
  open exactly as already stated; this review did not find any additional
  undisclosed debt.

## Retention decision

**KEEP. Final.** Every gate in this report is met at the current
authoritative `HEAD` (`f22bf8e`): >=1.5x `PHASE exhaustive` on `multiplier4`
cleared with large margin, identical outputs at 1/2/auto within the caveats
noted above, no representative case >5% slower at `HEAD`, and bounded peak
memory per the corrected I6 discussion above. I1, I2 (documentation scope)
and I3 are resolved by commits `0f5a2db` and `f22bf8e`: I1's overstated
test-binding claim is corrected in source, I3's missing `FunctionalMismatch`
fixture is added and controller-confirmed 23/23 passing at `f22bf8e`, and
I2's debt note carries the corrected "below 68 transitions" unmeasured range
directly in source comments/assert messages (the underlying small-sweep
benchmark itself remains open debt — see M7 above — but that debt was never
gate-blocking on its own). M1 above closes the `multiplier4` thread=2
evidence question, and M2 above records `d43e62f`'s single-threshold
deviation from plan Task 2 step 2 as a measured, gate-driven correction. The
final, independent exact-range review (Round 6, `61ffdb1..f22bf8e`) has now
completed: **SPEC PASS, CODE QUALITY PASS, KEEP**, no Critical or Important
findings. **No Opus review item remains outstanding for this report.**

The fallback below is retained for completeness only; it is not the live
decision. If a future review surfaces a Critical or Important finding this
report has not already addressed, revert all production commits from this
layer (`f899d6c`, `a6588b9`, `685dfa5`, `d43e62f`) and keep only the
test-only commits that remain valid against the reverted state
(`611349a`..`1e62683`, `b9e0c63`), per plan judgment at revert time. `94e171b`,
`0f5a2db` and `f22bf8e` are not independent of `d43e62f` in this fallback:
`94e171b` directly asserts the `certification_workers` policy constant values
`d43e62f` introduced, and `0f5a2db`/`f22bf8e` further document and pin those
same constants' rationale (their `FunctionalMismatch`-fixture portions are
general test coverage independent of `d43e62f` and could in principle be
retained on their own, but their doc-comment/assert-message portions
describing the manifest-threshold rationale cannot). If `d43e62f` is
reverted, `94e171b`'s assertions would fail or become vacuous against
different constant values, so at minimum `94e171b` must be reverted together
with `d43e62f`, and `0f5a2db`/`f22bf8e`'s constant-rationale wording would
need re-review against whichever constant value is retained.

## Cleanup before final commit

- ~~Revert the temporary `WORK sweep_budget` diagnostic eprintln and any
  temporary fingerprint-capture test edits in `certification.rs` /
  `seed.rs`.~~ **Done** — confirmed reverted by the clean `git diff --check`
  and `cargo clippy --lib --tests` results recorded in "Complete
  verification" above.
- ~~Re-run `git diff --check` and confirm the working tree is clean except for
  this report file before commit.~~ **Done** — see the `git diff --check`
  row and detail in "Complete verification" above.
