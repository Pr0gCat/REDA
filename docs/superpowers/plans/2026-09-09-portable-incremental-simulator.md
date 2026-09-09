# Portable Incremental Simulator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure exhaustive-certification CPU work and retain the smallest portable simulator optimizations that make `multiplier4` at least 1.5x faster end to end without changing any certified result or slowing `ripple_adder8`/`alu8` by more than 5%.

**Architecture:** Keep `run_indexed_chunks` as the only parallel executor and optimize the independent simulator owned by each vector. Temporarily measure clone/drive/settle/check and simulator work, remove the instrumentation, then test Layer 1A and Layer 1B independently; reach dirty-scoped scheduling only when fresh attribution proves it is the next material cost.

**Tech Stack:** Portable Rust standard library, existing REDA `Simulator`, deterministic certification workers, `std::time::Instant`, Cargo release tests, PowerShell benchmark orchestration.

**Spec:** `docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md`

## Global Constraints

- Baseline is exactly commit `225d3e28c41616d5f975b740df60d1352f4512a7` (`225d3e2`).
- Keep `run_indexed_chunks`; add no executor, dependency, GPU path, platform-specific branch or fixture-name special case.
- Every exhaustive mask starts from the pristine simulator with `work_done == 0`, an empty queue and baseline burnout history.
- Preserve tick priority, stable queue insertion, Torch-before-WallTorch-before-Repeater-before-Comparator-before-Lamp order and per-kind YZX order.
- Preserve ticks, blocks, occupied volume, static delay, candidate/world fingerprints, `QualityKey`, pinned IO, proposal traces, transition measurements, cap accounting, exact typed errors and lowest failing logical index.
- Fixed certification worker counts 1, 2 and 4 must produce identical results; one-worker `multiplier4` must stay within 5% of its own baseline.
- Final `multiplier4` median must be at most `119.9947335 s` for at least 1.5x over the fixed `179.9921003 s` baseline median.
- `ripple_adder8` median must be at most `15.36352587 s`; `alu8` median must be at most `54.17791974 s`.
- Retain Layer 1A and Layer 1B only after at least 10% lower `multiplier4` top-exhaustive wall time; retain Layer 2 only after at least 20% lower remaining top-exhaustive wall time.
- Worker-duration sums rank remaining CPU costs only. Retention and physical-limit claims use wall time only.
- Detailed `REDA_SIM_WORK_COUNTS` instrumentation is temporary and must be absent from the retained production diff and final executable.
- Run Cargo commands serially on Windows. An agent may edit or review while Cargo runs, but may not start another Cargo process.
- Claude Code model routing: Opus implements concurrency/semantic changes and performs final correctness review; Sonnet handles bounded source mapping, mechanical report work and over-engineering review; Fable does not write production code.

---

## File Map

- Modify temporarily, then restore `src/compile/fragment_synth/certification.rs`: collect deterministic per-vector clone/drive/settle/check worker durations.
- Modify temporarily, then restore `src/redstone/simulator/mod.rs`: collect settle, game-tick, event and component-scan work; later retain Layer 1B and, only if eligible, Layer 2.
- Modify temporarily, then restore `src/redstone/simulator/propagate.rs`: collect dust/topology work; later retain Layer 1A and, only if eligible, Layer 2's private dirty-origin result.
- Test in `src/redstone/simulator/propagate.rs`: pin Layer 1A's changed positions, final world and empty derived dirty set.
- Test in `src/redstone/simulator/mod.rs`: pin Layer 1B empty-tick behavior, bounded errors, observers and all delayed-component corner cases; add Layer 2 differential tests only if Layer 2 is eligible.
- Reuse `src/compile/resettle_differential.rs`: run all narrow and ignored wide full-resettle oracles without changing them.
- Reuse `src/compile/fragment_synth/certification.rs` tests: preserve ordered error reduction, manifest grouping and complete 1/2/4-worker certification identity.
- Reuse `src/compile/fragment_synth/seed.rs` ignored corpora: run the 16 extra, four large and four hierarchical 1/2/4-worker matrices and the three performance circuits selected by `REDA_EXTRA_CIRCUITS`.
- Keep `tests/fixtures/fragment_synth_baseline.json` and `tests/fixtures/fragment_synth_shipping.json` byte-identical.
- Create `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`: record raw samples, units, decisions, invariants, regressions and final verification.

### Task 1: Capture exhaustive internal work without retaining instrumentation

**Files:**
- Modify temporarily: `src/compile/fragment_synth/certification.rs:394-466, 1268-2405`
- Modify temporarily: `src/redstone/simulator/mod.rs:32-47, 270-536`
- Modify temporarily: `src/redstone/simulator/propagate.rs:28-102, 166-268`
- Create: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: existing `REDA_PHASE_TIMING`, `run_certification_chunks`, `run_indexed_chunks`, `Simulator::work_done`, `World::take_dirty` and `DustTopologyCache`.
- Produces temporarily: `SimulatorWorkCounts`, one `VectorWork` per exhaustive mask and deterministic aggregate stderr lines; produces permanently only the raw attribution section in the report.

- [ ] **Step 1: Write the failing aggregation arithmetic test**

Add a temporary test next to the existing certification worker tests. It must fail because `VectorWork::summary` does not yet exist:

```rust
#[test]
fn vector_work_summary_keeps_worker_units_and_logical_count() {
    let rows = [
        VectorWork { clone_ns: 3, drive_ns: 5, settle_ns: 7, check_ns: 11 },
        VectorWork { clone_ns: 13, drive_ns: 17, settle_ns: 19, check_ns: 23 },
    ];
    let summary = VectorWork::summary(&rows);
    assert_eq!(summary.count, 2);
    assert_eq!((summary.clone_sum_ns, summary.clone_max_ns), (16, 13));
    assert_eq!((summary.drive_sum_ns, summary.drive_max_ns), (22, 17));
    assert_eq!((summary.settle_sum_ns, summary.settle_max_ns), (26, 19));
    assert_eq!((summary.check_sum_ns, summary.check_max_ns), (34, 23));
}
```

Add `VectorWork` to the test module's explicit `use super::{...}` list before
running RED, so the failure is the missing type/implementation rather than an
unrelated name-resolution mistake.

- [ ] **Step 2: Run the RED test**

```powershell
cargo test --lib compile::fragment_synth::certification::tests::vector_work_summary_keeps_worker_units_and_logical_count -- --exact
```

Expected: compilation fails because `VectorWork` is absent.

- [ ] **Step 3: Add the minimum temporary counters**

In `certification.rs`, add local temporary structs using integer nanoseconds so sums saturate deterministically:

```rust
#[derive(Clone, Copy, Default)]
struct VectorWork { clone_ns: u128, drive_ns: u128, settle_ns: u128, check_ns: u128 }

#[derive(Clone, Copy, Default)]
struct VectorWorkSummary {
    count: usize,
    clone_sum_ns: u128, clone_max_ns: u128,
    drive_sum_ns: u128, drive_max_ns: u128,
    settle_sum_ns: u128, settle_max_ns: u128,
    check_sum_ns: u128, check_max_ns: u128,
}
```

`VectorWork::summary` loops once over the ordered `Vec<VectorWork>`, uses `saturating_add` for sums and `max` for maxima. In `certify_exhaustive_truth_with_threads`, compute:

```rust
let detailed = std::env::var_os("REDA_PHASE_TIMING").is_some()
    && std::env::var_os("REDA_SIM_WORK_COUNTS").is_some();
```

When `detailed` is true, time the existing four statements with `Instant::now()` and return `VectorWork`; otherwise return a zero row. Keep the closure's `Result` and `run_indexed_chunks` ordering unchanged. Print the summary only after every worker has joined. Labels must contain `worker_ns`, never `wall`.

Also wrap the one `fresh_simulator(...)` call in
`CompleteCandidateCertifier::certify_with_identity` with an outer `Instant` and
print `SIM_WORK baseline_build_wall_ns <value>` only under the same two-variable
gate. This span is construction wall time and is never added to worker-ns.

In `mod.rs`, temporarily add:

```rust
#[derive(Clone, Copy, Default)]
pub(crate) struct SimulatorWorkCounts {
    pub settle_iterations: u64,
    pub game_ticks: u64,
    pub due_events: u64,
    pub dirty_origins: u64,
    pub active_dust: u64,
    pub changed_dust: u64,
    pub topology_rebuilds: u64,
    pub topology_rebuild_worker_ns: u128,
    pub topology_cells: u64,
    pub topology_probes: u64,
    pub torch_predicates: u64,
    pub repeater_predicates: u64,
    pub comparator_predicates: u64,
    pub lamp_predicates: u64,
    pub torch_scan_worker_ns: u128,
    pub repeater_scan_worker_ns: u128,
    pub comparator_scan_worker_ns: u128,
    pub lamp_scan_worker_ns: u128,
}
```

Store it as `Option<SimulatorWorkCounts>` on `Simulator`, initialized only when both environment variables are present. Increment with already-computed lengths; time each existing topology-build and kind-scan boundary once. The exhaustive closure snapshots counts immediately after `baseline.clone()` and reports only the saturating delta after check. Do not use atomics or global mutable state.

Use `active_dust.len()` for topology cells and
`connections.iter().count()` once per direction before the neighbour loop for
probes; never add a counter increment inside the per-connection inner loop. Aggregate vector worker-ns is exactly the
sum of clone, drive, settle and check spans. `bits_of` and `enforce_event_cap`
remain explicitly unmeasured residual work.

- [ ] **Step 4: Run the GREEN test and focused certification tests**

```powershell
cargo test --lib compile::fragment_synth::certification::tests::vector_work_summary_keeps_worker_units_and_logical_count -- --exact
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: both commands exit 0; ordered error tests still report mask 0.

- [ ] **Step 5: Prebuild and capture the three-circuit attribution**

Before the expensive run, hand the complete temporary diff and the GREEN test
output to a fresh Opus reviewer. It must approve timer boundaries, saturating
aggregation, baseline-delta arithmetic, disabled-gate behavior and unchanged
ordered error reduction. Fix any blocking finding and rerun the focused tests;
do not benchmark unreviewed instrumentation.

Use one Cargo process and the exact ignored test:

```powershell
$test = 'compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan'
$env:REDA_PHASE_TIMING = '1'
$env:REDA_SIM_WORK_COUNTS = '1'
Remove-Item Env:REDA_CERT_THREADS -ErrorAction SilentlyContinue
Remove-Item Env:REDA_CERT_MEMORY_BYTES -ErrorAction SilentlyContinue
$env:CARGO_TARGET_DIR = (Join-Path $env:TEMP 'reda-sim-attribution-225d3e2')
cargo test --release --lib $test --no-run
foreach ($case in 'multiplier4','ripple_adder8','alu8') {
    $env:REDA_EXTRA_CIRCUITS = $case
    cargo test --release --lib $test -- --exact --ignored --nocapture --test-threads=1 2>&1 |
        Tee-Object -FilePath (Join-Path $env:TEMP "reda-$case-attribution.txt")
    if ($LASTEXITCODE -ne 0) { throw "$case attribution failed" }
}
```

Record exact raw lines in the report. For each circuit, separately list outer construction wall time, `PHASE exhaustive` wall time, vector count, each worker-ns sum/max/count, simulator work counts, the already measured CPU-core samples and peak working set. Rank candidates only by same-unit worker-ns shares.

- [ ] **Step 6: Remove every detailed counter before optimization**

Delete `VectorWork`, `VectorWorkSummary`, `SimulatorWorkCounts`, the arithmetic test, every `REDA_SIM_WORK_COUNTS` read and every temporary print. Keep only the report. Verify removal and the unchanged phase-only build:

```powershell
rg -n "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
if ($LASTEXITCODE -eq 0) { throw 'temporary instrumentation remains' }
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
git diff --check
```

Expected: `rg` finds nothing, tests pass, and only the report/spec/plan plus later retained code remain.

- [ ] **Step 7: Commit the approved design and attribution evidence**

```powershell
git add -- docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md docs/superpowers/plans/2026-09-09-portable-incremental-simulator.md docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md
git diff --cached --check
git commit -m "docs: specify exhaustive simulator acceleration"
```

The commit contains no temporary Rust instrumentation. Record the temporary
reviewer's verdict and the exact removal search in the report and SDD ledger.

### Task 2: Eliminate cached dust self-dirty work (Layer 1A)

**Files:**
- Modify: `src/redstone/simulator/propagate.rs:166-268, 540-856`
- Reuse without edits: `src/compile/resettle_differential.rs`
- Update: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: `recompute_dust_strengths_cached(&mut World, &mut Arc<DustTopologyCache>) -> Vec<Position>` and `World::take_dirty()`.
- Produces: the same signature, changed-position order and final world, with no derived dust write-back entries left dirty.

- [ ] **Step 1: Add the failing cached-path oracle**

In `propagate.rs`'s existing test module, add:

```rust
#[test]
fn cached_recompute_returns_changes_without_redirtying_its_own_writeback() {
    let mut source = World::new(5, 3, 3);
    source.set(0, 1, 0, redstone_block());
    source.set(1, 1, 0, dust());
    source.set(2, 1, 0, dust());

    let mut expected = source.clone();
    let mut expected_changed = recompute_dust_strengths(&mut expected);
    let mut actual = source;
    let mut cache = Arc::new(DustTopologyCache::default());
    let mut actual_changed = recompute_dust_strengths_cached(&mut actual, &mut cache);

    actual_changed.sort();
    expected_changed.sort();
    assert_eq!(actual_changed, expected_changed);
    for flat in 0..expected.cells().len() {
        let (x, y, z) = expected.decode(flat);
        assert_eq!(actual.get(x, y, z), expected.get(x, y, z));
    }
    assert!(actual.take_dirty().is_empty(), "cached write-back must consume only its own dirt");
}
```

- [ ] **Step 2: Run the RED test**

```powershell
cargo test --lib redstone::simulator::propagate::tests::cached_recompute_returns_changes_without_redirtying_its_own_writeback -- --exact
```

Expected before implementation: the final assertion fails because changed wires remain dirty.

- [ ] **Step 3: Make the one-call-site change**

Replace the tail of `recompute_dust_strengths_cached` with:

```rust
let active_dust = cache.active_positions(world, &dirty);
let changed = recompute_active_dust(world, &active_dust, Some(cache));
let _derived_writeback = world.take_dirty();
changed
```

Do not modify public `recompute_dust_strengths`, `World::set`, palette/index maintenance or topology epochs. Do not add a writer abstraction.

- [ ] **Step 4: Run focused and mandatory full-resettle tests**

```powershell
cargo test --lib redstone::simulator::propagate::tests -- --nocapture
cargo test --lib redstone::simulator::differential::tests -- --nocapture
cargo test --lib compile::resettle_differential::the_reported_stale_dust_case_settles_clean -- --exact
cargo test --lib compile::resettle_differential::and4s_full_sweep_is_differential_clean -- --exact
cargo test --release --lib compile::resettle_differential -- --ignored --nocapture --test-threads=1
```

Expected: all narrow tests and the six-condition, negotiated-plan, 240-transition, isolation and injected-isolation ignored tests pass.

- [ ] **Step 5: Apply the Layer 1A wall-time gate**

Create one detached clean baseline worktree and two isolated target directories;
refuse to reuse an existing path:

```powershell
$baselineRepo = Join-Path $env:TEMP 'reda-baseline-225d3e2'
$baselineTarget = Join-Path $env:TEMP 'reda-target-baseline-225d3e2'
$candidateTarget = Join-Path $env:TEMP 'reda-target-incremental-candidate'
foreach ($path in $baselineRepo,$baselineTarget,$candidateTarget) {
    if (Test-Path -LiteralPath $path) { throw "benchmark path already exists: $path" }
}
git worktree add --detach $baselineRepo 225d3e2
```

Prebuild clean baseline and candidate executables in their separate targets.
Run `multiplier4`, `ripple_adder8` and `alu8` in `B1,C1,C2,B2,B3,C3` order
with the exact command in the spec, no `REDA_SIM_WORK_COUNTS`, and no build
between samples. Record raw `PHASE exhaustive` and `CIRCUIT` intervals.

Every B sample must execute in the detached baseline checkout:

```powershell
Push-Location $baselineRepo
try {
    $env:CARGO_TARGET_DIR = $baselineTarget
    $env:REDA_EXTRA_CIRCUITS = $case
    cargo test --release --lib $test -- --exact --ignored --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "baseline $case failed" }
} finally { Pop-Location }
```

Every C sample executes from the active worktree with
`$env:CARGO_TARGET_DIR = $candidateTarget`. Extract top exhaustive as the final
`PHASE exhaustive` before the case's `CIRCUIT` line; check the immediately
following `WORK exhaustive_vectors` line as a guard.

KEEP only when `median(candidate top exhaustive) <= 0.90 * median(baseline top exhaustive)`, both representative medians stay under their fixed ceilings and every semantic test passes. Otherwise remove the test and implementation with `apply_patch`, rerun the focused tests, and record `REVERT`.

- [ ] **Step 6: Commit only a passing Layer 1A**

```powershell
git add -- src/redstone/simulator/propagate.rs docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md
git diff --cached --check
git commit -m "perf: discard derived dust dirty entries"
```

If the gate failed, commit only the report evidence later; do not retain counter-only code.

- [ ] **Step 7: Re-attribute a retained Layer 1A before selecting Layer 1B**

Only after KEEP, temporarily add back the four `VectorWork` spans and optional
`SimulatorWorkCounts` fields from Task 1. Run `multiplier4` once with both
diagnostic environment variables, record the new clone/drive/settle/check,
dust/topology and per-kind component worker-ns shares, then remove all temporary
code. This evidence must still name component scans as material before Task 3
is attempted. Confirm the source search for `REDA_SIM_WORK_COUNTS`, `VectorWork`,
`SimulatorWorkCounts` and `worker_ns` is empty afterward.

If Layer 1A is REVERTED, use Task 1's unchanged-baseline attribution for the
same decision: attempt Task 3 only when empty-tick component scans were named
as removable work. Otherwise skip directly to Task 4's measured branch logic.

### Task 3: Skip mismatch scans on empty game ticks (Layer 1B)

**Files:**
- Modify: `src/redstone/simulator/mod.rs:32-47, 365-536, 732-1502`
- Update: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: existing `work_done` deltas around `advance_one_tick` and `advance_one_tick_bounded`.
- Produces: unchanged public APIs and observer/cap behavior; repeated settle scans occur only after a tick processed at least one scheduled event.

- [ ] **Step 1: Add a test-only scan-round counter and RED test**

Under `#[cfg(test)]`, add `component_scan_rounds: u64` to `Simulator`, initialize it to zero and increment it once at the start of `settle_from_current_state`. Add a private test-only accessor. This field does not exist in production builds.

Add a delayed-repeater test that drives a delay-4 repeater, calls `run_until_stable(20)`, asserts eight game ticks and one processed event, then asserts exactly two component-scan rounds: the mandatory initial scan and the post-event scan. Before the loop change, the assertion must report more than two rounds.

- [ ] **Step 2: Run the RED test**

```powershell
cargo test --lib redstone::simulator::tests::empty_delay_ticks_do_not_repeat_component_scans -- --exact --nocapture
```

Expected: assertion failure showing redundant scans.

- [ ] **Step 3: Reshape each stable loop once**

For both stable APIs, call `settle_from_current_state()` once before entering the loop. After each successful advance, compare `work_done` with its pre-advance value:

```rust
let work_before = self.work_done;
self.advance_one_tick();
game_ticks_run += 1;
if self.work_done != work_before {
    self.settle_from_current_state();
}
```

Use the same ordering for the bounded path, but return `WorkLimitExceeded` immediately when `advance_one_tick_bounded` refuses the oversized due bucket; do not increment `game_ticks_run`, sample the observer or settle after that error. Keep `step()` unchanged.

- [ ] **Step 4: Add exact semantic regression cases**

In the same test module, add one compact test per distinct boundary:

- a scheduled no-op still causes the post-event scan because `work_done` moved;
- burnout expiry and a locked repeater match their existing final world/tick/work behavior;
- an oscillator returns the exact existing `Diverged { game_ticks, pending }` payload;
- an observer samples every advanced tick, including empty ticks;
- `run_until_stable_bounded` with a due bucket larger than remaining allowance returns exact `WorkLimitExceeded { used, limit }`, leaves the queue tick at the refused boundary and performs no post-error observer/dust scan;
- a delayed lamp that becomes desired-on again is not dropped.

Use existing `torch`, `repeater`, `lamp`, `attach_observer`, `observations` and `TickQueue` fixtures; add no clock or scheduler abstraction.

- [ ] **Step 5: Run GREEN and simulator/certification gates**

```powershell
cargo test --lib redstone::simulator::tests -- --nocapture
cargo test --test simulator_circuits -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: all pass, including exact cap and lowest-mask tests.

- [ ] **Step 6: Apply the Layer 1B wall-time gate**

Repeat the clean `B1,C1,C2,B2,B3,C3` three-circuit protocol with phase-only
binaries. Each B sample runs inside `$baselineRepo` under `Push-Location` with
`CARGO_TARGET_DIR=$baselineTarget`; each C sample runs in the active worktree
with `CARGO_TARGET_DIR=$candidateTarget`. KEEP only when top exhaustive is at
least 10% lower than the immediately preceding retained revision, both
representative medians stay below their fixed ceilings and semantic output is
exact. Otherwise remove Layer 1B and its test-only scan counter and record
`REVERT`.

- [ ] **Step 7: Commit only a passing Layer 1B**

```powershell
git add -- src/redstone/simulator/mod.rs docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md
git diff --cached --check
git commit -m "perf: skip scans on empty simulator ticks"
```

Continue immediately to Task 4 so the retained Layer 1B is re-attributed before
any larger scheduler change is selected.

### Task 4: Re-attribute the retained stack and make the closed next-layer decision

**Files:**
- Modify temporarily, then restore: `src/compile/fragment_synth/certification.rs`, `src/redstone/simulator/mod.rs`, `src/redstone/simulator/propagate.rs`
- Update: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: the retained post-Layer-1 revision.
- Produces: a measured branch decision: stop at target, Layer 2 eligible, topology-local rebuild needs a separate approved design, per-worker reset needs a separate approved design, or another measured cost must be investigated.

- [ ] **Step 1: Reinsert the same temporary measurement surface**

Temporarily add `VectorWork { clone_ns, drive_ns, settle_ns, check_ns }` and a
summary containing count plus sum/max for each field to `certification.rs`.
Wrap only `baseline.clone()`, `drive_vector`, `settle` and `check_outputs` inside
the existing exhaustive worker closure; reduce rows only after
`run_certification_chunks` returns.

Temporarily add an optional `SimulatorWorkCounts` to `Simulator` with settle
iterations, game ticks, due events, dirty origins, active/changed dust,
topology rebuild count/worker-ns/cells/probes, and per-kind predicate
count/worker-ns. Initialize it only when both diagnostics are enabled, snapshot
after clone, use saturating deltas and print after ordered reduction. No atomic,
shared mutable counter or wall/worker unit conversion is allowed.

- [ ] **Step 2: Re-run the three-circuit attribution serially**

Use the exact Task 1 command with a fresh target directory named `reda-sim-attribution-retained`. Record all raw output in the report, then remove every counter, env read, temporary test and print. Confirm with:

```powershell
rg -n "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
if ($LASTEXITCODE -eq 0) { throw 'temporary instrumentation remains' }
git diff --check
```

- [ ] **Step 3: Take exactly one decision from measured units**

Apply these branches in order:

1. If final end-to-end median is already `<= 119.9947335 s`, skip Layer 2 and continue to Task 6.
2. If topology rebuild is at least 10% of aggregate vector worker-ns, continue through Tasks 6 and 7 for the retained stack, then write a separate local-rebuild spec for user approval before changing topology behavior.
3. Else if component scans are at least 10% of aggregate vector worker-ns, continue to Task 5.
4. Else if cloning is at least 10% of aggregate vector worker-ns, continue through Tasks 6 and 7 for the retained stack, then write a separate per-worker-reset spec for user approval.
5. Else continue through Tasks 6 and 7, then measure the next named worker cost; do not invent a cache.

Do not call a worker-ns percentage a wall-time or physical upper bound.

### Task 5: Add dirty-scoped component scheduling only when eligible (Layer 2)

**Files:**
- Modify: `src/redstone/simulator/mod.rs:8-47, 270-720, 732-1502`
- Modify: `src/redstone/simulator/propagate.rs:166-268`
- Update: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: dirty flat indices taken by cached dust recompute and changed dust positions; arbitrary external edits through `Simulator::world_mut()`.
- Produces privately: ordered pending origin/change sets, five YZX candidate sets and a full-scan flag; public recompute and simulator APIs remain unchanged.

- [ ] **Step 1: Add the RED differential harness**

Add a `#[cfg(test)] force_full_component_scan: bool` switch and a helper that snapshots queue entries in insertion order. Run paired scoped/full simulators tick by tick over normal, delayed, locked, burnout, oscillating and lamp-cancellation worlds. After each tick assert identical world cells, queue snapshot, current tick, `work_done`, observations and exact terminal result.

Add two boundary tests. The topology test snapshots the four fields for every
active component and changed dust cell, exercises each private apply method plus
cached dust recompute, and compares the complete inferred vectors:

```rust
#[test]
fn internal_writes_preserve_component_topology_tuple() {
    let (mut simulator, positions) = topology_invariant_world();
    let before = positions.iter().map(|position| {
        let state = simulator.world().get(position.x, position.y, position.z);
        (state.kind, state.name.clone(), state.half, state.facing)
    }).collect::<Vec<_>>();

    exercise_every_internal_writer(&mut simulator);

    let after = positions.iter().map(|position| {
        let state = simulator.world().get(position.x, position.y, position.z);
        (state.kind, state.name.clone(), state.half, state.facing)
    }).collect::<Vec<_>>();
    assert_eq!(after, before);
}

#[test]
fn world_mut_forces_a_full_scan_after_a_remote_shape_edit() {
    let mut simulator = bent_run_simulator();
    simulator.run_until_stable(50).expect("initial run settles");
    simulator.full_component_scan = false;
    simulator.world_mut().set(REMOTE_BEND.x, REMOTE_BEND.y, REMOTE_BEND.z, dust());
    assert!(simulator.full_component_scan);
}
```

Define `topology_invariant_world`, `exercise_every_internal_writer`,
`bent_run_simulator` and `REMOTE_BEND` in the same test module from the existing
`torch`, `wall_torch`, `repeater`, `comparator`, `lamp`, `dust`, `stone` and
`named` constructors. The first helper places the five component kinds and one
dust run far enough apart to avoid coupling; the writer helper calls the four
private `apply_*_tick` methods and `recompute_dust_strengths`. The bent-run test
uses the same three/four-hop one-way shape as the existing differential tests;
the paired scoped/full harness then proves final behavior, while this direct
assertion proves the mutable-access boundary itself.

The tests must fail before scoped fields/behavior exist.

- [ ] **Step 2: Extend only the private cached recompute result**

Add a private result used only by `Simulator`:

```rust
pub(super) struct CachedDustResult {
    pub(super) changed: Vec<Position>,
    pub(super) dirty_origins: Vec<usize>,
}

pub(super) fn recompute_dust_strengths_cached(
    world: &mut World,
    cache: &mut Arc<DustTopologyCache>,
) -> CachedDustResult
```

Narrow the existing cached function from `pub(crate)` to `pub(super)` with the
type so no wider interface exposes a more-private result. Have it capture
`dirty` before active-set lookup and return both vectors internally. Keep public
`recompute_dust_strengths(world) -> Vec<Position>` unchanged. Layer 1A still
consumes derived write-back dirt only after `changed` is captured.

- [ ] **Step 3: Add the minimum deterministic candidate state**

Add seven `BTreeSet<usize>` fields and one `full_component_scan: bool` to `Simulator`: pending dirty origins, pending changed dust, standing torch, wall torch, repeater, comparator and lamp. `new` and `world_mut` clear pending sets and set `full_component_scan = true`; while full scan is pending, recomputes need not populate pending sets.

Enumerate each exact Manhattan radius-two ball with bounded loops:

```rust
for dx in -2i32..=2 {
    for dy in -2i32..=2 {
        for dz in -2i32..=2 {
            if dx.abs() + dy.abs() + dz.abs() > 2 { continue; }
            let position = Position::new(origin.x + dx, origin.y + dy, origin.z + dz);
            let Some(flat) = self.world.index(position.x, position.y, position.z) else {
                continue;
            };
            match self.world.get(position.x, position.y, position.z).kind {
                BlockKind::Torch => { standing_torches.insert(flat); }
                BlockKind::WallTorch => { wall_torches.insert(flat); }
                BlockKind::Repeater => { repeaters.insert(flat); }
                BlockKind::Comparator => { comparators.insert(flat); }
                BlockKind::Lamp => { lamps.insert(flat); }
                _ => {}
            }
        }
    }
}
```

Do not reuse `propagate::positions_within_two_hops`; it is a duplicate-producing frontier for a different dust-seeding purpose.

- [ ] **Step 4: Preserve mandatory retained candidates and order**

Each kind pass visits its `BTreeSet` in flat-index order. Retain any component whose visit did not conclude "examined and matched", including mismatched, locked and already-scheduled components. Merge consumed origins and changed dust across recomputes in `advance_one_tick`, `advance_one_tick_bounded` and `settle_from_current_state`; clear them only after all five passes finish. Full scans keep Torch before WallTorch before Repeater before Comparator before Lamp.

- [ ] **Step 5: Run focused differential and full simulator gates**

```powershell
cargo test --lib redstone::simulator::tests -- --nocapture
cargo test --lib redstone::simulator::propagate::tests -- --nocapture
cargo test --lib redstone::simulator::differential::tests -- --nocapture
cargo test --test simulator_circuits -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: every tick-level pair and all existing simulator/certification tests pass.

- [ ] **Step 6: Apply the Layer 2 wall-time gate**

Run the clean three-circuit protocol in `B1,C1,C2,B2,B3,C3` order. Each B
sample runs inside `$baselineRepo` under `Push-Location` with
`CARGO_TARGET_DIR=$baselineTarget`; each C sample runs in the active worktree
with `CARGO_TARGET_DIR=$candidateTarget`. KEEP only when remaining
`multiplier4` top-exhaustive median falls by at least 20%, representative
medians remain under their ceilings and all semantic gates pass. Otherwise
remove every Layer 2 field/helper/test while preserving previously retained
layers, rerun focused tests and record `REVERT`.

- [ ] **Step 7: Commit only a passing Layer 2**

```powershell
git add -- src/redstone/simulator/mod.rs src/redstone/simulator/propagate.rs docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md
git diff --cached --check
git commit -m "perf: scope simulator component scans"
```

- [ ] **Step 8: Re-attribute a retained Layer 2 and remove diagnostics**

Temporarily wrap the exhaustive closure's clone, drive, settle and check calls
with per-vector `Instant`s and re-add the optional simulator counts for settle
iterations, ticks/events, dust/topology work and per-kind scan work. Run all
three circuits once with both diagnostic variables, record the ordered sums,
maxima and counts, then remove every temporary field, print and env read. The
retained source must again produce no match for:

```powershell
rg -n "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
if ($LASTEXITCODE -eq 0) { throw 'temporary instrumentation remains' }
```

### Task 6: Prove the final performance and semantic matrix

**Files:**
- Update: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`
- Verify unchanged: `tests/fixtures/fragment_synth_baseline.json`, `tests/fixtures/fragment_synth_shipping.json`

**Interfaces:**
- Consumes: final retained source and clean `225d3e2` baseline binaries.
- Produces: raw `B1,C1,C2,B2,B3,C3` evidence, ratio-of-medians verdicts and exact semantic equality evidence.

- [ ] **Step 1: Run final alternating release benchmarks**

For each of `multiplier4`, `ripple_adder8`, `alu8`, use separate prebuilt target directories and run in this literal order:

```text
B1, C1, C2, B2, B3, C3
```

Clear `REDA_CERT_THREADS`, `REDA_CERT_MEMORY_BYTES` and `REDA_SIM_WORK_COUNTS`; keep only `REDA_PHASE_TIMING=1`. Report `median(B) / median(C)` as the retention speedup. Report `B1/C1`, `B2/C2`, `B3/C3` only as drift diagnostics.

Run every B sample under `Push-Location $baselineRepo` with
`CARGO_TARGET_DIR=$baselineTarget`, and every C sample from the active worktree
with `CARGO_TARGET_DIR=$candidateTarget`. Extract top exhaustive as the final
`PHASE exhaustive` before the case's `CIRCUIT` line.

- [ ] **Step 2: Run the one-worker portability gate**

Repeat three `multiplier4` samples per side in the same order with:

```powershell
$env:REDA_CERT_THREADS = '1'
```

Require `median(candidate) <= 1.05 * median(baseline)` and exact quality/fingerprints.

- [ ] **Step 3: Run focused ordered-certification gates**

```powershell
Remove-Item Env:REDA_EXTRA_CIRCUITS, Env:REDA_CERT_THREADS, Env:REDA_CERT_MEMORY_BYTES, Env:REDA_SIM_WORK_COUNTS -ErrorAction SilentlyContinue
cargo test --lib compile::fragment_synth::certification::tests::manifest_source_groups_preserve_every_transition_and_index -- --exact
cargo test --lib compile::fragment_synth::certification::tests::certified_candidate_is_identical_at_one_two_and_four_workers -- --exact
cargo test --lib compile::fragment_synth::certification::tests::exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count -- --exact
cargo test --lib compile::fragment_synth::certification::tests::exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count -- --exact
```

- [ ] **Step 4: Run all fixed 1/2/4-worker corpora serially**

```powershell
Remove-Item Env:REDA_EXTRA_CIRCUITS, Env:REDA_CERT_THREADS, Env:REDA_CERT_MEMORY_BYTES, Env:REDA_SIM_WORK_COUNTS -ErrorAction SilentlyContinue
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_extra_circuit_agrees_across_certification_thread_counts -- --exact --ignored --nocapture --test-threads=1
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_large_circuit_agrees_across_certification_thread_counts -- --exact --ignored --nocapture --test-threads=1
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_agrees_across_certification_thread_counts -- --exact --ignored --nocapture --test-threads=1
```

Expected: 16 extra, four large and four hierarchical cases produce exact equality at 1/2/4 workers.

- [ ] **Step 5: Run the pinned IO and six-case acceptance contracts**

```powershell
Remove-Item Env:REDA_EXTRA_CIRCUITS, Env:REDA_CERT_THREADS, Env:REDA_CERT_MEMORY_BYTES, Env:REDA_SIM_WORK_COUNTS -ErrorAction SilentlyContinue
cargo test --release --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --exact --nocapture
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo test --release --test fragment_synth_baseline -- --nocapture
```

Expected: all eleven literal IO positions remain exact; baseline/shipping fixture files remain byte-identical.

Run the actual six-case, five-budget acceptance evaluator into a fresh temporary
directory rather than overwriting checked fixtures:

```powershell
$acceptanceDir = Join-Path $env:TEMP ("reda-sim-acceptance-" + [guid]::NewGuid())
New-Item -ItemType Directory -LiteralPath $acceptanceDir | Out-Null
cargo run --release --bin fragment_acceptance -- `
    --baseline tests/fixtures/fragment_synth_baseline.json `
    --output (Join-Path $acceptanceDir 'report.json') `
    --shipping-source (Join-Path $acceptanceDir 'shipping_config.rs') `
    --shuffle-seed 0x5245444120260831
if ($LASTEXITCODE -ne 0) { throw 'six-case acceptance harness failed' }
```

Record all six case outcomes and compare the generated report's deterministic
fields with `tests/fixtures/fragment_synth_shipping.json`; timing fields are
reported separately and are not used as semantic equality.

- [ ] **Step 6: Record the final decision honestly**

If final `multiplier4` is at least 1.5x and every gate passes, mark the retained stack `KEEP`. If the target is missed, claim a physical limit only if the report has an evidence-backed wall-time lower bound for every remaining phase whose sum exceeds `119.9947335 s`; otherwise continue from fresh attribution. If such a strict bound exists, replay each passing optimization independently against clean `225d3e2` and keep only the single largest general end-to-end winner.

### Task 7: Full verification, independent review and final commit

**Files:**
- Modify only if review finds a real defect: retained simulator/certification files
- Finalize: `docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md`
- Include: `docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md`
- Include: `docs/superpowers/plans/2026-09-09-portable-incremental-simulator.md`

**Interfaces:**
- Consumes: final retained diff and all Task 6 evidence.
- Produces: a clean, reviewed, committed branch with no temporary diagnostics.

- [ ] **Step 1: Run the repository-wide gate**

```powershell
Remove-Item Env:REDA_EXTRA_CIRCUITS, Env:REDA_CERT_THREADS, Env:REDA_CERT_MEMORY_BYTES, Env:REDA_SIM_WORK_COUNTS -ErrorAction SilentlyContinue
bash ./check.sh
cargo clippy --all-targets --all-features
git diff --check
```

Expected: all commands exit 0. Run them serially.

- [ ] **Step 2: Prove temporary and fixture-specific code is absent**

```powershell
rg -n "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
if ($LASTEXITCODE -eq 0) { throw 'temporary instrumentation remains' }
git diff -- tests/fixtures/fragment_synth_baseline.json tests/fixtures/fragment_synth_shipping.json
git status --short
```

Expected: first search and fixture diff are empty; status lists only intended source/docs.

- [ ] **Step 3: Run independent reviews**

Give one fresh Claude Opus reviewer the approved spec, complete diff and exact test/benchmark evidence. Require a verdict on false certification, tick/queue ordering, cap/error ordering, dirty consumption, full-scan fallback and 1/2/4-worker determinism. Give a separate Sonnet reviewer the same diff for Ponytail over-engineering review. Fix Critical/Important findings, then rerun affected tests.

- [ ] **Step 4: Finalize report and commit**

The report must include commands/environment, raw samples, medians, paired drift ratios, wall-vs-worker units, CPU/clone/settle counts, peak working set, exact quality/fingerprints, every KEEP/REVERT decision and reviewer verdicts.

```powershell
git add -- src/redstone/simulator/mod.rs src/redstone/simulator/propagate.rs docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md docs/superpowers/plans/2026-09-09-portable-incremental-simulator.md docs/superpowers/reports/2026-09-09-portable-incremental-simulator.md
git diff --cached --check
git status --short
git commit -m "perf: accelerate exhaustive simulation"
```

Omit any source file not retained. Confirm the commit and clean worktree:

```powershell
git show --stat --oneline HEAD
git status --short
```

After all evidence is captured, remove the detached baseline worktree with its
literal validated path:

```powershell
$expectedBaseline = Join-Path $env:TEMP 'reda-baseline-225d3e2'
$resolvedBaseline = (Get-Item -LiteralPath $expectedBaseline).FullName
$expectedFullName = (Get-Item -LiteralPath (Split-Path -Parent $expectedBaseline)).FullName + '\reda-baseline-225d3e2'
if ($resolvedBaseline -ne $expectedFullName) { throw "unexpected baseline path: $resolvedBaseline" }
git worktree remove $resolvedBaseline
```
