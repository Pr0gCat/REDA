# Portable Parallel Certification Implementation Plan

> Execute in the existing `topology-aware-seed-v2-6f8f7e` worktree after the
> foundation report is committed. Opus 5 is the production implementer and
> high-risk reviewer. Sonnet may audit bounded tests and benchmark arithmetic.
> Fable 5.1 must not write production code.

**Goal:** Parallelize exhaustive truth certification on ordinary CPUs while
preserving byte-identical results, lowest-index failures and one
compilation-wide worker budget.

**Non-goals:** no GPU, executor, dependency, persistent pool, simulator rewrite,
proposal parallelism or routing change.

**Retention gate:** at least 1.5x `PHASE exhaustive` speedup on `multiplier4`,
identical outputs at thread counts 1, 2 and auto, no representative case over
5% slower, and bounded peak memory. Revert the layer if any gate fails.

## Task 1: Freeze ordered parallel reduction

**Files:**

- Modify: `src/compile/fragment_synth/certification.rs`

1. Add RED tests for one shared indexed-chunk reducer:
   - success values return in increasing logical index, never completion order;
   - the lowest-index typed error wins over a later error or panic;
   - if no earlier typed error exists, the lowest-index worker panic resumes;
   - `threads=0`, `threads=1` and an empty input remain on the caller thread,
     safe and deterministic.
2. Run only the new test names and capture the expected missing-helper failures.
3. Implement the smallest generic scoped-thread helper around the existing
   `chunks(div_ceil)` and ordered handle collection. Reuse `std::thread::scope`;
   add no queue, channel, pool or dependency.
4. Route the manifest parallel path through it. Preserve its serial fast path,
   transition indices, measurements and current error/panic order.
5. Run certification tests and commit:

```powershell
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
git add -- src/compile/fragment_synth/certification.rs
git diff --cached --check
git commit -m "refactor: share deterministic certification chunks"
```

## Task 2: Parallelize exhaustive vectors

**Files:**

- Modify: `src/compile/fragment_synth/certification.rs`

1. Add RED tests for an exception-safe scoped certification-thread override:
   nested scopes restore the previous value after success and panic. Use that
   scope to run the same real certified candidate at worker counts 1, 2 and 4
   and compare the complete `CertifiedCandidate`: candidate, emitted-world
   fingerprint, equivalence-certificate fingerprint, timing graph, complete
   manifest/fingerprint, measurements, metrics and errors. A cap refusal must
   report the same lowest failing mask at every worker count; Task 1's test-only
   barrier/channel test, not scheduler luck, proves completion-order inversion.
2. After the RED tests and a draft of steps 3-5 exist, but before freezing or
   committing the production cutoff, run a bounded release timing probe over
   increasing real exhaustive-vector counts at one and two workers. Record the
   first repeatable crossover and use that single measured value as the minimum
   items-per-worker threshold in the shared helper, so both exhaustive and
   manifest sweeps use it. Exhaustive vectors are the lighter unit, making the
   same threshold conservative for manifest transitions. Add a RED test that
   work below it stays on the caller thread. Do not add an adaptive scheduler
   or stage-specific policy. Remove any temporary probe-only test before the
   production commit; retain only the measured value and its behavioral test.
   For non-empty work the final helper uses one rule: actual workers are the
   minimum of requested workers and `items / minimum_items_per_worker`, clamped
   to at least one. Empty work reports zero and executes no closure.
3. Split the canonical mask range into stable contiguous chunks through the
   Task 1 helper. Each worker processes masks in increasing order and owns one
   simulator at a time. Keep `bits_of`, fresh initial world semantics, drive,
   settle, event-cap and output checks unchanged.
4. Return/reduce only `()` plus the canonical vector count; do not retain one
   simulator or vector per mask.
5. Add one minimal closure-scoped thread-local override in `certification.rs`.
   Expose only the closure helper to sibling fragment-synth modules
   (`pub(super)`); keep its reset guard private. The guard restores the previous
   value on normal return or panic and cannot escape to another thread. This is
   runtime scheduling state and is not fingerprinted. Without a scope, use the
   existing `REDA_CERT_THREADS` override and the single worker-policy helper from
   Task 3, then clamp by vector count. Keep empty, below-threshold and one-worker
   paths free of thread creation.
6. Under `REDA_PHASE_TIMING`, add stable
   `WORK exhaustive_workers N` and `WORK manifest_workers N` lines. Diagnostics
   report the actual post-threshold worker count and do not enter fingerprints.
7. Run focused tests, then all certification tests, and commit:

```powershell
cargo test --lib exhaustive -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
git add -- src/compile/fragment_synth/certification.rs
git diff --cached --check
git commit -m "perf: parallelize exhaustive certification"
```

## Task 3: Enforce one compilation-wide worker budget

**Files:**

- Modify: `src/compile/fragment_synth/certification.rs`
- Modify: `src/compile/fragment_synth/api.rs`
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`

1. Add RED tests that worker threads do not inherit an accidental wider budget
   and that hierarchy thread counts 1, 2 and auto select the intended active
   dimension.
2. Parse and clamp `REDA_CERT_THREADS` once at the public hierarchy entry; the
   `threads` argument to `compile_hierarchical_with_threads` is the whole
   compile's budget. Auto and the explicit override both clamp to at least one
   and the host's `available_parallelism`; do not preserve the old
   machine-derived 12-worker ceiling, read CPU model names or add a
   fingerprinted config field. Auto additionally clamps to a byte-based memory
   ceiling so a 64- or 128-core host cannot multiply cloned worlds without
   bound. `World` is dense, so estimate one worker as
   `size_of::<u32>() * volume * WORLD_COPY_HEADROOM`, using `World::size()`;
   compute the ceiling as `CERT_WORKER_MEMORY_BUDGET_BYTES / per_worker_bytes`,
   clamped to at least one. Keep the two measured constants module-private and
   non-fingerprinted; allow `REDA_CERT_MEMORY_BYTES` to override the byte budget.
   An explicit `REDA_CERT_THREADS` remains a deliberate operator/test override
   and clamps only to `available_parallelism`. Exhaustive and manifest sweeps
   must share this policy; do not create a second parser or clamp path.
3. Parse the same policy once at the public `compile_fragment_synth` entry and
   scope the complete flat compile too. Cover all five hierarchy paths with the
   same closure helper: the flat fast path, concurrent leaf workers, sequential
   intermediate parents, top seed and proposals. Flat and sequential paths
   receive the whole budget. With more than one ready leaf, each leaf worker
   receives `1`; with exactly one ready leaf, compile it on the caller thread
   with the whole budget. Do not add a field to `SeedServices` merely to
   transport runtime scheduling state.
4. Clamp leaf workers to `min(budget, ready_leaves)`: zero leaves execute
   nothing, one leaf compiles on the caller thread, and only more than one uses
   `thread::scope`. The one-leaf caller path must use the same ordered reduction,
   not return its failure directly. Add behavioral tests for a one-core compile
   and a 32-worker budget with two ready leaves; neither may spawn a useless
   worker. Preserve the current failure contract: attempt every queued leaf and
   report the lexicographically lowest failing module, never the first wall-clock
   failure.
5. Rename the manifest-only lock/helper to certification-sweep names and share
   them with exhaustive. Keep this process-wide lock after the matrix: TLS
   controls nesting within one compile, while the lock prevents two independent
   compiles from each opening the full worker count. Remove it only with a later
   process-wide budget arbiter; do not introduce a semaphore or pool here.
6. Extend the existing one-vs-many hierarchy test to compare complete metrics,
   emitted fingerprint, trace, stop reason, evaluation/cap counters and typed
   failures, not only candidate/case fingerprints.
   Rename the existing bounded worker-policy test and replace its old
   `(available=32, explicit=32) == 12` assertion with `== 32`. Also cover
   `(available=32, auto)` with its memory ceiling and
   `(available=1, explicit=32) == 1`.
7. Run API, hierarchy and certification tests, then commit:

```powershell
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
cargo test --lib compile::fragment_synth::api::tests -- --nocapture
cargo test --lib compile::fragment_synth::hierarchy_api::tests -- --nocapture
git add -- src/compile/fragment_synth/certification.rs src/compile/fragment_synth/api.rs src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "perf: coordinate certification worker budget"
```

## Task 4: Thread-count acceptance and refusal matrix

**Files:**

- Modify only if needed: existing tests in
  `src/compile/fragment_synth/certification.rs`,
  `src/compile/fragment_synth/hierarchy_api.rs`,
  `src/compile/fragment_synth/seed.rs` and
  `tests/build_circuit_pins.rs`

Run serially. Use `REDA_CERT_THREADS=1`, `2`, then remove the variable for auto.
For each setting, preserve candidate/case/world fingerprints, `CandidateMetrics`,
equivalence-certificate and complete manifest fingerprints, worst indices,
proposal trace, terminal classification, quality, evaluation count and cap-work
counters. `SynthesisResult` does not expose per-transition manifest measurements;
cite Task 2's direct `CertifiedCandidate` comparison for that row instead of
inventing a production result field.

Run the full 1/2/auto correctness matrix over:

- the six-case `fragment_acceptance` corpus;
- all 16 `every_extra_circuit` cases;
- all four `every_large_circuit` cases;
- all four hierarchy cases (`ripple_adder8`, `alu4_full`, `multiplier4` and
  `alu8`) through
  `every_hierarchical_circuit`;
- the pinned seven-segment IO contract.

Do not infer equality from the current human-readable corpus lines: they omit
fingerprints and traces. Reuse the Task 2 thread scope in ignored test harnesses
that compile a thread-1 reference and directly compare its stable result fields
with thread 2 and auto. Keep the corpus constructors shared with the existing
tests instead of copying their case lists. This is test-only plumbing, not a new
production result type or API. A test already proving a row may be cited instead
of duplicated, but no corpus group may be omitted. Give the flat extra, flat
large and hierarchical ignored tests the shared substring
`certification_thread_counts`, so the command below cannot silently select zero
tests.

Run the resulting ignored matrix tests serially. Also run the public environment
integration for each of `1`, `2` and auto (auto means
`Remove-Item Env:REDA_CERT_THREADS`) against a fresh `$parallelCertDir`; the six-
case acceptance JSON and shipping source must be byte-identical across all
three settings, and the pinned contract must pass each time:

```powershell
cargo test --release --lib certification_thread_counts -- --ignored --nocapture
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output "$parallelCertDir/acceptance.json" --shipping-source "$parallelCertDir/shipping_config.rs" --shuffle-seed 0x5245444120260831
cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --nocapture
```

Required refusal fixtures:

- lowest exhaustive output mismatch;
- simulator event cap;
- divergence;
- worker panic ordering through the shared reducer.

Do not add a timing assertion. Commit only real missing coverage; if existing
tests already prove a row, cite them in the report instead of duplicating them.

## Task 5: Benchmark and retention decision

**Files:**

- Create: `docs/superpowers/reports/2026-09-07-portable-parallel-certification.md`

1. After release build, run three samples each at `REDA_CERT_THREADS=1`, `2`
   and auto. Record raw `PHASE exhaustive`, worker count, end-to-end time,
   fingerprints, quality and peak working set. Also record `per_worker_bytes`,
   the auto memory ceiling and actual worker count per case. Peak memory must
   track `workers * per_worker_bytes` within `WORLD_COPY_HEADROOM`; if it does
   not, raise that measured headroom rather than lowering workers ad hoc.
2. The primary targeted fixture is budget-zero `multiplier4`; also run
   `ripple_adder8` and `alu8` once per setting as balanced/routing-heavy guards.
3. Compute targeted speedup against the same retained foundation revision and
   same thread setting. Do not mix build time or redstone ticks into compiler
   wall time.
4. Record the host's logical parallelism and exact reproducible commands. This
   host may establish the layer's retention gate, but the report must not claim
   cross-machine scaling until the same release matrix has evidence from a
   second distinct machine, as required by the spec.
5. Record `cargo test --lib` wall time before and after the process-wide lock is
   extended to exhaustive work, so serialization between independent test
   compiles is visible. Then run complete verification serially:

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

6. Give the complete diff, plan, spec and report to a fresh Opus 5 reviewer.
   Require explicit findings on ordered error/panic reduction, cap accounting,
   thread-budget restoration, nested oversubscription, memory, pinned IO and
   benchmark arithmetic. Fix every Critical/Important finding and rerun affected
   tests.
7. KEEP only if every gate at the top of this plan passes; otherwise revert all
   production commits from this layer. Commit the final report separately.

## Stop conditions

- A one-worker result differs from the retained serial foundation.
- Any worker count changes a fingerprint, metric, trace, first typed error,
  cap counter or quality.
- Peak memory grows with host core count rather than the byte budget, exceeds the
  measured headroom model, or a representative case regresses more than 5%.
- Scoped thread creation remains at least 10% of exhaustive time after useful
  work is parallel; only then write a separate persistent-pool design.
