# Five Low-Cost Optimization Passes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to execute this plan task-by-task, and superpowers:test-driven-development for Tasks 1-5. The controller dispatches a fresh implementer and a fresh reviewer per task, exactly as the sub-skill specifies. An individual implementer or reviewer must not spawn nested agents of its own; it executes its task, writes the full report to the SDD report file, and commits that task.

**Goal:** Evaluate exactly five low-cost optimization candidates in one wave, each against its own cheap gate, and retain only the measured winners. A candidate that misses its opportunity signal or its threshold is fully removed before the next candidate starts. Zero retained candidates is a legitimate outcome.

**Architecture:** Nothing new is built. Candidate 1 memoizes `BlockFlags` inside the existing `Palette`, which already owns every interned state. Candidate 2 adds two permanent `PHASE` lines beside the existing ones and, only if they justify it, one structurally coupled `RoutedParent` sidecar carried by the `Arc` the reused plan already hands back. Candidate 3 hoists the consumer index `merge_isolation_mask` already builds. Candidate 4 adds one more stage to the existing finite `HierarchicalProposalStream`, in the same shape the existing stages use. Candidate 5 extends the existing `relocate_refresh` with a bounded fallback, reusing `branches_carry_through` and `prune_route`.

**Tech Stack:** Rust standard library, existing REDA world/simulator/hierarchy/route/timing types, existing certification path, serial Cargo on PowerShell.

**Spec:** `docs/superpowers/specs/2026-09-11-five-low-cost-optimization-passes.md`. That spec supersedes exactly two claims of `docs/superpowers/specs/2026-09-10-pareto-tick-density-passes.md` -- "there is no sixth stage" and "relocation is straight-only". Every other contract in the 2026-09-10 spec remains binding and this plan may not weaken one.

---

## Fixed constraints

- Baseline HEAD is `6c9f8b50cb00759a07c28f839c9180dac69696aa`, branch
  `claude/topology-aware-seed-v2-6f8f7e`, clean worktree.
- No dependency, GPU path, machine-specific tuning, or fixture-specific
  production branch.
- **Cargo is strictly serialized.** Exactly one Cargo command runs at a time,
  always through `Invoke-Capped` below.
- **Every command is hard-capped at 10 minutes.** A capped command is a failed
  measurement, never a passing gate.
- **The whole execution wave is budgeted at 60 minutes of measured command
  time.** Sum every `Invoke-Capped` elapsed time in the report as you go. Many
  sub-10-minute commands must not accumulate into another multi-hour run. At the
  budget, stop, record what is measured, and mark the rest as not attempted.
- Each candidate has its own cheap gate. A missed opportunity signal or a missed
  threshold means **full revert/removal before the next candidate starts**.
- The final validation -- four-circuit semantic check, pinned IO and worker
  1/2/4 -- runs **once, in Task 6**, over the retained set only, and stays inside
  the same caps. **Exhaustion (`u64::MAX`) is never run for `multiplier4` or for
  all four circuits**: `multiplier4` exhaustion is measured at over 68 minutes
  and cannot fit a 600 s cap.
- Preserve semantics, first-error order, ordered proposal stream, fingerprints,
  traces, exact certification and manifests, pinned IO, and worker determinism.
- No temporary counter, timer, feature flag or disabled branch survives the
  wave. The only permanent survivors of an instrumentation kind are the two
  `PHASE` lines in Task 2, justified by the existing `REDA_PHASE_TIMING`
  diagnostics they join.
- **TDD evidence must capture a real expected RED before production code.** A
  test that was green before the production change is characterization and must
  be labelled as such in the report.
- Each task's implementer writes the full report to
  `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md` and
  commits its own task. Implementers and reviewers do not spawn nested agents.
- Ponytail full: smallest diff, reuse existing helpers, no speculative
  abstraction.
- Format only touched Rust files with `rustfmt`; never repository-wide
  `cargo fmt`. No lint or maintenance cleanup inside a candidate commit.

## The capped runner

Define this once per shell session. Every Cargo command and every measurement in
this plan goes through it, one at a time.

The child's exit status must reach the parent. `Invoke-Expression` swallows it
unless it is emitted deliberately, so the job prints one unambiguous sentinel
line as its last output and the parent throws on anything nonzero.

The child's stderr must reach the transcript too: every `PHASE` and `WORK`
line is `eprintln!`. Inside `Start-Job`, `Invoke-Expression $c 2>&1` merges
nothing from a native child, so the runner wraps the command in a script block
and redirects that block. Callers never append `2>&1`; the runner owns it.

```powershell
function Invoke-Capped {
    param(
        [Parameter(Mandatory)][string]$Command,
        [string]$Log
    )
    $job = Start-Job -ScriptBlock {
        param($c, $d)
        Set-Location -LiteralPath $d
        $global:LASTEXITCODE = 0
        # Native stderr (PHASE/WORK lines are eprintln!) is merged by the
        # runner, not by the caller: a `2>&1` on Invoke-Expression itself does
        # not reach a native child inside Start-Job.
        Invoke-Expression "& {`n$c`n} 2>&1"
        $code = if ($null -eq $LASTEXITCODE) { 0 } else { $LASTEXITCODE }
        "REDA_CAPPED_EXIT=$code"
    } -ArgumentList $Command, (Get-Location).Path

    if (-not (Wait-Job -Job $job -Timeout 600)) {
        Stop-Job -Job $job
        Remove-Job -Job $job -Force
        throw "CAPPED at 600s, measurement failed: $Command"
    }
    $out = Receive-Job -Job $job
    Remove-Job -Job $job -Force

    $sentinel = @($out | Where-Object { "$_" -match '^REDA_CAPPED_EXIT=(\d+)$' })
    if ($sentinel.Count -ne 1) { throw "no exit sentinel: $Command" }
    $code = [int]([regex]::Match("$($sentinel[0])", '^REDA_CAPPED_EXIT=(\d+)$').Groups[1].Value)
    $body = $out | Where-Object { "$_" -notmatch '^REDA_CAPPED_EXIT=\d+$' }
    if ($Log) { $body | Set-Content -LiteralPath $Log }
    if ($code -ne 0) { throw "exit $code : $Command" }
    $body
}
```

`REDA_CAPPED_EXIT=` is reserved: no test, transcript or fixture may print it.
`Start-Job` does not inherit environment variables, so every `REDA_*` variable is
set **inside** the `-Command` string, and no `REDA_*` variable is ever left set
in the interactive shell.

## The ripple measurement command

**Ripple budget-0 paired sample.** The harness certifies each case at **two**
budget points, so one command compiles `ripple_adder8` **twice**. One capped
command is therefore **one paired sample**, read as the **maximum** top
`PHASE manifest` and the **maximum** `RETENTION` `wall_ms` within that command.
Never treat the two compiles inside one command as two independent samples.

This is Candidate 1's gate and the source of Candidate 4's single certified run.
It is **not** Candidate 2's gate: budget-0 ripple has zero plan-reuse hits.

```powershell
$ripple = @'
$env:REDA_EXTRA_CIRCUITS='ripple_adder8'
$env:REDA_RETENTION_BUDGET='0'
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture
'@
```

**Reading one paired sample.** `$Log` is one capped command's transcript. Wall
time comes from the `RETENTION` lines the harness prints, **not** from libtest's
`test result:` duration, which includes compilation and harness overhead.

```powershell
function Read-RippleSample {
    param([Parameter(Mandatory)][string]$Log)
    $lines = Get-Content -LiteralPath $Log
    $maxOf = {
        param($pattern)
        $values = @($lines | Select-String -Pattern $pattern -AllMatches |
            ForEach-Object { $_.Matches } | ForEach-Object { [int]$_.Groups[1].Value })
        if ($values.Count -eq 0) { $null } else { ($values | Measure-Object -Maximum).Maximum }
    }
    $retention = @($lines | Select-String -Pattern '^RETENTION ' | ForEach-Object { $_.Line })
    $wall = @($retention | ForEach-Object {
        [int][regex]::Match($_, 'wall_ms=(\d+)').Groups[1].Value })
    [pscustomobject]@{
        TopManifestMs = & $maxOf '^PHASE manifest (\d+)$'
        TopPrunableMs = & $maxOf '^PHASE prunable (\d+)$'
        TopFlattenMs  = & $maxOf '^PHASE flatten (\d+)$'
        # Only the RETENTION lines carry a wall time; nothing else is read.
        MaxWallMs     = if ($wall.Count -eq 0) { $null } else { ($wall | Measure-Object -Maximum).Maximum }
        Quality       = @($retention | ForEach-Object {
            [regex]::Match($_, 'settle=(\d+) blocks=(\d+) volume=(\d+) static=(\d+)').Value })
        Fingerprints  = @($retention | ForEach-Object {
            [regex]::Match($_, 'case=(\S+) candidate=(\S+)').Value })
        Work          = @($lines | Select-String -Pattern '^WORK ' | ForEach-Object { $_.Line })
        Retention     = $retention
    }
}
```

`Work` is compared **as an ordered sequence**, not as a set: the same lines in a
different order are a regression.

Medians are over **three capped commands**, i.e. three paired samples. The
already-recorded pair is **sample 1**: top `PHASE manifest` max `3749` ms,
`RETENTION` max `wall_ms` `9789`. Task 0 captures **two more** commands so the
median is over three. Record every sample, not just the median.

## Source map

- `src/redstone/world/storage.rs:10-30,31-76,117-185,234-285` -- `World`, its
  palette index, `get`, `set`, `from_parts`, `dust_topology_changed`.
- `src/redstone/world/palette.rs:11-52` -- `Palette::intern`, `get`, `entries`.
- `src/redstone/rules/taxonomy.rs:31-77,93` -- `BlockFlags`, `flags_of`.
- `src/redstone/simulator/connectivity.rs:17-40` -- `is_conductive`,
  `supports_dust_step`.
- `src/redstone/simulator/propagate.rs:433-440` -- `block_signal_at`.
- `src/compile/fragment_synth/hierarchy_api.rs:450-501,503-510,519-538,839-866,900-1013,1015-1120` --
  `union_and_certify`, `prunable_parent_routes`, `refresh_descriptors`,
  `block_pull_x_proposal`, `HierarchicalProposalStream` and its stage selection.
- `src/compile/fragment_synth/topology.rs:544-587` -- `merge_isolation_mask` and
  `MergeMaskError`.
- `src/compile/fragment_synth/instance_graph.rs:221-233,234-248,796-852` --
  `InstanceGraph::with_variants`, `instantiate_gates`.
- `src/compile/primitive_graph.rs:484-520,595-620` -- `shared_merge_branches`,
  `expand_with_selection`.
- `src/compile/fragment_synth/route_opt.rs:40-71,96-281,283-360,363-415` --
  `prune_route`, `relocate_refresh`, `relocation_offsets`,
  `branches_carry_through`.
- `src/compile/fragment_synth/route_opt.rs:465-590` -- existing route fixture
  helpers `set`, `kind_at`, `linear_relocation_route`, `divert`,
  `remap_anchors`, `onto_z_axis`, `step_by`, `repeater_anchors`. Reuse these;
  do not add a second fixture builder.
- `src/compile/fragment_synth/seed.rs:4805-4900,4958-4988,5140-5260` --
  retention harness, `print_retention_record`, worker 1/2/4 matrix.
- `tests/build_circuit_pins.rs:625-805` --
  `hierarchy_with_a_child_preserves_requested_pins_through_exhaustion`.
- `src/circuits/seven_segment.rs:56` -- `build_seven_segment_netlist`, the real
  flat netlist Candidate 3's probe uses.
- `src/bin/fragment_baseline.rs`, `src/bin/fragment_acceptance.rs` -- flat
  control.

---

## Task 0: Verify the baseline and open the report

**Files:**

- Create: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Confirm provenance before any measurement. `HEAD` is **not** required to be
  the source baseline: docs commits land on top of it. What is required is that
  the source baseline is an ancestor, the worktree is clean, and `src` is
  byte-identical to the baseline.

```powershell
$baseSrc = '6c9f8b50cb00759a07c28f839c9180dac69696aa'
git merge-base --is-ancestor $baseSrc HEAD; if ($LASTEXITCODE -ne 0) { throw 'baseline is not an ancestor of HEAD' }
if ((git status --porcelain)) { throw 'worktree is not clean' }
git diff --exit-code $baseSrc HEAD -- src; if ($LASTEXITCODE -ne 0) { throw 'src differs from the baseline' }
git rev-parse --abbrev-ref HEAD
```

Expected: branch `claude/topology-aware-seed-v2-6f8f7e`, no throw. Anything else
stops the wave.

- [ ] Capture **two more** ripple budget-0 paired samples, so every later median
  is over three capped commands. The already-recorded pair is sample 1: top
  `PHASE manifest` max `3749` ms, `RETENTION` max `wall_ms` `9789`.

```powershell
2..3 | ForEach-Object {
    $log = Join-Path $env:TEMP "reda-wave-baseline-sample$_.txt"
    Invoke-Capped -Command $ripple -Log $log
    Read-RippleSample -Log $log
}
```

- [ ] Assert each transcript reproduces the recorded baseline verbatim: quality
  `settle=608 blocks=70603 volume=1123332 static=678`, and both fingerprint
  strings from the spec's baseline block.

```powershell
$want = @(
    'settle=608 blocks=70603 volume=1123332 static=678',
    'b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573',
    'a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2'
)
2..3 | ForEach-Object {
    $text = (Get-Content -LiteralPath (Join-Path $env:TEMP "reda-wave-baseline-sample$_.txt")) -join "`n"
    $want | ForEach-Object { if ($text -notmatch [regex]::Escape($_)) { throw "MISSING: $_" } }
}
```

- [ ] Capture the three cheap focused gates once, so a later regression has a
  clean comparison. Recorded values: merge correctness 18 tests total across its
  two commands / 0.277 s, Pull-X 2 passed + 1 ignored / 0.672 s, refresh 9
  passed + 2 ignored / 0.728 s.

```powershell
Invoke-Capped -Command 'cargo test --release --lib merge_isolation -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --lib block_pull_x'
Invoke-Capped -Command 'cargo test --lib refresh_relocation'
```

- [ ] Capture the reuse fixture once. Recorded: 1.98 s warm, 75.197 s cold.

```powershell
Invoke-Capped -Command 'cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan'
```

- [ ] Write the report skeleton: baseline table (branch, worktree, all five
  recorded signals, both fingerprints, the third sample), the five candidates as
  `PENDING`, the wave verdict line `retained 0 of 5 so far`, and the exact
  `Invoke-Capped` definition used. Commit only the report.

```powershell
git add -- docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "docs: record five-candidate wave baseline"
git rev-parse HEAD
```

Record that commit SHA in the report as **the wave start commit**. Every
candidate's revert target is the commit standing when that candidate starts.

---

## Task 1: Candidate 1 -- palette-indexed `BlockFlags` memo

**Files:**

- Modify: `src/redstone/world/palette.rs:11-52`.
- Modify: `src/redstone/world/storage.rs:117-131,143-185`.
- Modify: `src/redstone/simulator/connectivity.rs:17-40`.
- Modify: `src/redstone/simulator/propagate.rs:433-440`.
- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Write failing unit tests. Every assertion is behavioural -- compare values
  the public API returns. No test may grep source or assert on field layout.

  In `palette.rs`'s test module:

  - `flags_are_interned_alongside_every_state` -- `intern` dust, a repeater,
    stone and air; `Palette::flags(index)` equals `flags_of` of the state `get`
    returns at that index, for every index.
  - `flags_of_an_unknown_index_is_none` -- `Palette::flags(len)` is `None`, the
    same shape `get` already uses.

  In `storage.rs`'s test module:

  - `flags_at_agrees_with_flags_of_on_every_placed_cell` -- place dust, a
    repeater, stone, glass and a hopper; every `flags_at` equals
    `flags_of(world.get(..))`.
  - `flags_at_out_of_bounds_equals_the_in_bounds_air_cell` -- an out-of-bounds
    read equals both `world.flags_at` of an untouched in-bounds cell and
    `flags_of(&BlockState::air())`. Air's flags legitimately are
    `BlockFlags::NONE`; do not assert the result differs from `NONE`.
  - `flags_at_sees_a_palette_entry_interned_after_construction` -- `set` a state
    whose palette index is new, then read it back through `flags_at`.
  - `from_parts_answers_flags_at_for_a_palette_that_had_no_air` -- build via
    `from_parts` with a palette lacking air, then assert both an in-bounds stone
    cell and an out-of-bounds read.

- [ ] Run RED:

```powershell
Invoke-Capped -Command 'cargo test --lib redstone::world -- --nocapture'
```

Expected RED: compilation fails, `no method named flags` on `Palette` and
`no method named flags_at` on `World`. Record the exact failure text in the
report.

- [ ] Implement the minimum. **`Palette` owns the memo**, because `intern` is its
  only mutator and the parallel-vector invariant then has exactly one place it
  can be violated. `World` holds no second copy and no second invariant.

```rust
// palette.rs
entries: Vec<BlockState>,
flags: Vec<BlockFlags>,   // flags[i] == flags_of(&entries[i]), by construction
lookup: HashMap<BlockState, u32>,
```

`intern` pushes `flags_of(&state)` in the same branch that pushes `entries`.
Nothing else mutates either vector. Add:

```rust
#[inline]
pub fn flags(&self, index: u32) -> Option<BlockFlags> {
    self.flags.get(index as usize).copied()
}
```

`World::flags_at` delegates, using the cell's palette index in bounds and
`air_index` out of bounds -- the same fallback `get` already uses:

```rust
#[inline]
pub fn flags_at(&self, x: i32, y: i32, z: i32) -> BlockFlags {
    let index = match self.index(x, y, z) {
        Some(flat) => self.cells[flat],
        None => self.air_index,
    };
    self.palette
        .flags(index)
        .expect("palette index out of range")
}
```

A `debug_assert_eq!` against `flags_of(self.get(..))` inside `flags_at` is
optional; add it only if it earns its place, and nowhere else.

Leave `dust_topology_key` and `dust_topology_changed` exactly as they are,
including the kind/half/name fast path: they compare two `BlockState`s, not two
world cells.

- [ ] Switch only the three hot neighbour queries: `connectivity.rs`'s
  `is_conductive` and `supports_dust_step`, and `propagate.rs`'s
  `block_signal_at` guard. Every other `flags_of` call site stays.

- [ ] Run GREEN plus the simulator and world suites:

```powershell
Invoke-Capped -Command 'cargo test --lib redstone::world -- --nocapture'
Invoke-Capped -Command 'cargo test --lib redstone::simulator -- --nocapture'
```

`dust_topology_epoch_ignores_dynamic_state_and_tracks_connectivity_flags` is
characterization here: it was green before this change and must stay green.

- [ ] Run the retention gate: three ripple budget-0 paired samples, one per
  capped command.

```powershell
1..3 | ForEach-Object {
    $log = Join-Path $env:TEMP "reda-wave-c1-sample$_.txt"
    Invoke-Capped -Command $ripple -Log $log
    Read-RippleSample -Log $log
}
```

- [ ] Decide, and say which branch was taken in the report.

  - **GO** requires all of: median `TopManifestMs` at least **1.5x** faster than
    the baseline median (baseline median / candidate median >= 1.5); median
    `MaxWallMs` no more than **5%** slower than the baseline median; all four
    quality fields, the ordered `WORK` sequence and both fingerprints identical
    to the baseline.
  - **Anything else is NO-GO.** Call-count reasoning, a faster micro-benchmark,
    or "it must be faster asymptotically" do not substitute for the phase number.

- [ ] On NO-GO, remove the candidate completely and prove it:

```powershell
git checkout -- src/redstone/world/palette.rs src/redstone/world/storage.rs src/redstone/simulator/connectivity.rs src/redstone/simulator/propagate.rs
git status --porcelain
Invoke-Capped -Command 'cargo test --lib redstone::world -- --nocapture'
```

`git status --porcelain` must show only the report. Record the candidate as
**attempted, NO-GO**, with all three samples and the computed ratios.

- [ ] On GO, format only the touched files, then commit the candidate and its
  report section together.

```powershell
rustfmt src/redstone/world/palette.rs src/redstone/world/storage.rs src/redstone/simulator/connectivity.rs src/redstone/simulator/propagate.rs
git add -- src/redstone/world/palette.rs src/redstone/world/storage.rs src/redstone/simulator/connectivity.rs src/redstone/simulator/propagate.rs docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "perf: memoize block flags by palette index"
```

- [ ] On NO-GO, commit the report alone.

```powershell
git add -- docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git commit -m "docs: record block flags memo as no-go"
```

---

## Task 2: Candidate 2 -- `prunable_parent_routes` diagnostics, then a conditional sidecar

This candidate is a re-proposal of the reverted `14aead2` / `95b6b9d`. The
revert's scope stays prohibited: **no `ModuleCompileContext`, no flattening
hoist, no restructuring of `union_and_certify`'s call graph.** Also prohibited:
a mutable one-entry cache, an `Arc::ptr_eq` or `Arc::as_ptr` cache key, and any
production probe counter. The coupling is structural -- the value travels with
the plan -- so there is no cache to invalidate.

**Files:**

- Modify: `src/compile/fragment_synth/hierarchy_api.rs`. Changing
  `HierarchicalCandidate::planned` to `Arc<RoutedParent>` touches **every**
  existing `planned.candidate` / `planned.block_offsets` read, not just
  `union_and_certify`. At the source baseline those are `:347` and `:382`
  (the two `Arc::new` plan sites), `:380` (the `Arc::clone` reuse branch),
  `:461` and `:464` (`union_and_certify`), `:519-538` (`refresh_descriptors`,
  reading at `:524`), the field declaration near `:872`, and the existing tests
  at `:3060-3115` (reads at `:3083` and `:3102`). Update every one; a symbol
  search for those two field paths is the authority, not this list.
- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

### Step A -- diagnostics, unconditionally

- [ ] Add two `PHASE` lines beside the existing `PHASE union`
  (`hierarchy_api.rs:479-481`), in its exact shape and under its exact
  `REDA_PHASE_TIMING` guard: `PHASE flatten` around the `module_flattening` call
  at `:462` and `PHASE prunable` around the `prunable_parent_routes` call at
  `:464`. These are permanent and are the only instrumentation this wave may
  leave behind. They carry no test: they are diagnostics, and the transcript
  below is their evidence. Say that explicitly in the report rather than claiming
  TDD for them.

- [ ] Measure on the **plan-reuse fixture**, not on ripple budget 0. Budget-0
  ripple never reuses a plan, so its `PHASE prunable` lines cannot show what this
  candidate removes. `unchanged_block_placements_reuse_the_incumbent_plan`
  (`hierarchy_api.rs:4047`) is the one existing test that drives the reuse
  branch. Run three capped commands.

```powershell
$reuse = @'
$env:REDA_PHASE_TIMING='1'
cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture --test-threads=1
'@
1..3 | ForEach-Object {
    $log = Join-Path $env:TEMP "reda-wave-c2-diag$_.txt"
    Invoke-Capped -Command $reuse -Log $log
    $values = @((Get-Content -LiteralPath $log) | Select-String -Pattern '^PHASE prunable (\d+)$' |
        ForEach-Object { [int]$_.Matches[0].Groups[1].Value })
    [pscustomobject]@{ Lines = $values; AggregateMs = ($values | Measure-Object -Sum).Sum }
}
```

The **relevant prunable cost** of one command is that command's `AggregateMs`:
the total `prunable` time across the fixture's compiles, which is the upper
bound on what the sidecar can remove. Record the individual lines too.

- [ ] Commit the diagnostics on their own, so the decision below is made from a
  clean, committed revision.

```powershell
rustfmt src/compile/fragment_synth/hierarchy_api.rs
git add -- src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "feat: report flatten and prunable phase timings"
```

### Step B -- the kill switch

- [ ] If the median relevant prunable cost is **<= 5 ms**, the sidecar is
  **NO-GO**, immediately. Do not write it. The two `PHASE` lines stay, record the
  three measured aggregates and the verdict in the report, commit the report, and
  go to Task 3.

```powershell
git add -- docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git commit -m "docs: record prunable sidecar as no-go under five milliseconds"
```

### Step C -- the sidecar, only if the median relevant prunable cost is > 5 ms

- [ ] Write the failing test in `hierarchy_api.rs`'s test module:

  - `an_unchanged_placement_reuses_the_routed_parent_and_its_prunable_routes` --
    drive the existing reuse path and assert `Arc::ptr_eq` between the
    incumbent's and the proposal's `Arc<RoutedParent>`, **and** that the two
    carry the same `prunable_routes`. Identity of the `Arc` is the whole claim;
    there is no counter to assert on.
  - `a_moved_block_builds_a_new_routed_parent_with_its_own_prunable_routes` --
    a proposal that really moves a block gets a different `Arc`, and its
    `prunable_routes` equals what `prunable_parent_routes` computes for its own
    planned routes.

- [ ] Run RED:

```powershell
Invoke-Capped -Command 'cargo test --lib routed_parent -- --nocapture'
```

Expected RED: `RoutedParent` does not exist.

- [ ] Implement the structural coupling. One small private struct, and the
  sidecar is computed at the one place a plan is constructed:

```rust
/// A routed parent plus the facts derived from it that never change while
/// it does not. Reusing the plan reuses these by construction, so there is
/// no cache and nothing to invalidate.
struct RoutedParent {
    planned: PlannedParent,
    prunable_routes: BTreeSet<RouteId>,
}

impl RoutedParent {
    fn new(planned: PlannedParent) -> Self {
        let prunable_routes = prunable_parent_routes(&planned.candidate.routes);
        Self { planned, prunable_routes }
    }
}
```

`HierarchicalCandidate::planned` becomes `Arc<RoutedParent>`. Both existing
`Arc::new(..)` plan sites (`hierarchy_api.rs:347` and `:382`) become
`Arc::new(RoutedParent::new(..))`; the reuse branch at `:380` keeps its
`Arc::clone` and now carries the sidecar for free. `union_and_certify` reads
`planned.prunable_routes` instead of calling `prunable_parent_routes`, and every
existing `planned.candidate` / `planned.block_offsets` read becomes
`planned.planned.candidate` / `planned.planned.block_offsets`. Nothing else
moves: `prunable_parent_routes` keeps its signature and body, and
`union_and_certify` keeps its call graph.

**The `PHASE prunable` line moves with the computation.** Because the call leaves
`union_and_certify` for `RoutedParent::new`, the timing must move there too --
same `REDA_PHASE_TIMING` guard, same `PHASE prunable` name, so the diagnostic
stays permanent and keeps measuring the thing it names. A compile that reuses a
plan then emits **no** `PHASE prunable` line at all, because no computation
happens. That is the intended effect, not a lost measurement.

- [ ] Run GREEN and the hierarchy suite:

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture'
```

- [ ] Re-run the three reuse-fixture commands from Step A. Post-change,
  `AggregateMs` sums only the surviving new-plan computations; reused plans
  contribute nothing because they emit no line. That drop **is** the measurement.
  GO requires a measurable drop in the median relevant prunable cost, with the
  fixture's own assertions still passing. Otherwise NO-GO: `git checkout --` the
  file back to the Step A commit and prove `git status --porcelain` shows only
  the report.

- [ ] Commit.

```powershell
rustfmt src/compile/fragment_synth/hierarchy_api.rs
git add -- src/compile/fragment_synth/hierarchy_api.rs docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "perf: carry prunable routes with the routed parent"
```

---

## Task 3: Candidate 3 -- hoisted merge consumer index

**Files:**

- Modify: `src/compile/fragment_synth/topology.rs:544-587`.
- Modify: `src/compile/fragment_synth/instance_graph.rs:796-852`.
- Modify: `src/compile/primitive_graph.rs:484-520,595-620`.
- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Write the disposable probe first, as an ignored test in
  `instance_graph.rs`'s test module, and name it so its disposability is
  unmistakable:

  - `disposable_merge_consumer_index_probe` -- builds the real flat netlist from
    `crate::circuits::seven_segment::build_seven_segment_netlist()` and
    `Library::default_library()` (both confirmed present at the source
    baseline), then times **20 repeats** of
    `InstanceGraph::with_variants(&netlist, &library, &BTreeMap::new(), &[])`
    and 20 repeats of `crate::compile::primitive_graph::expand(&netlist, &library)`,
    printing one `PROBE batch=... repeats=20 total_ms=...` line for each. No
    synthetic netlist: the gate is about a real batch.

- [ ] Record three baseline probe runs **before** any production change.

```powershell
1..3 | ForEach-Object {
    Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::instance_graph::tests::disposable_merge_consumer_index_probe -- --ignored --exact --nocapture'
}
```

- [ ] Write failing unit tests for the hoist:

  - In `topology.rs`: `merge_isolation_mask_with_index_matches_the_public_function`
    -- for every merge gate of the seven-segment netlist, the indexed helper and
    the public function return equal masks.
  - In `topology.rs`: `merge_isolation_mask_keeps_unknown_then_not_merge_then_too_many_inputs`
    -- an out-of-range gate index yields `UnknownGate`, an existing non-merge
    yields `NotMerge`, and a 65-input merge yields `TooManyInputs`, each checked
    with an index already built, proving precedence is evaluated before the index
    is consulted.
  - In `instance_graph.rs`: `instantiate_gates_reports_the_same_first_failing_gate_with_a_shared_index`
    -- a netlist with two separately-failing gates returns the error of the
    **lower** gate index, identical to today's value.

- [ ] Run RED:

```powershell
Invoke-Capped -Command 'cargo test --release --lib merge_isolation -- --nocapture --test-threads=1'
```

Expected RED: the indexed helper does not exist.

- [ ] Implement the minimum. No new struct: a free function returning the map
  `merge_isolation_mask` already builds, plus one indexed variant.

```rust
pub(crate) fn build_consumer_index(lowered: &Netlist) -> HashMap<&str, Vec<usize>> {
    /* today's loop, verbatim, once */
}

pub(crate) fn merge_isolation_mask_with_index(
    lowered: &Netlist,
    gate_index: GateIndex,
    consumers: &HashMap<&str, Vec<usize>>,
) -> Result<InputMask, MergeMaskError> {
    /* today's body below the index build, verbatim */
}
```

`merge_isolation_mask` keeps its exact public signature and becomes
`build_consumer_index(lowered)` followed by `merge_isolation_mask_with_index`, so
the two paths cannot drift. The three validation steps -- unknown gate, not a
merge, too many inputs -- stay at the top of `merge_isolation_mask_with_index`,
in that order, before the index is read. `instantiate_gates`,
`expand_with_selection` and `shared_merge_branches` each call
`build_consumer_index` once before their gate loop and pass the borrow in; none
of them changes its visit order or its first-error return.

- [ ] Run GREEN. The merge correctness gate is **exactly these two commands**,
  18 tests in total at the measured baseline. Do not substitute a looser filter.

```powershell
Invoke-Capped -Command 'cargo test --release --lib merge_isolation -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::topology -- --nocapture'
Invoke-Capped -Command 'cargo test --lib compile::primitive_graph -- --nocapture'
```

The two gate commands must still total **18 passed** and stay in the same order
of magnitude as the recorded 0.277 s.

- [ ] Run three post-change probe runs with the same command as above.

- [ ] Decide. **GO requires both**: median total at least **1.5x** faster on the
  targeted batch, **and** an absolute saving of at least **100 ms** across the
  20 repeats. Correctness alone does not qualify. The quadratic-to-linear
  argument alone does not qualify.

- [ ] On NO-GO, revert every production file and the probe:

```powershell
git checkout -- src/compile/fragment_synth/topology.rs src/compile/fragment_synth/instance_graph.rs src/compile/primitive_graph.rs
git status --porcelain
```

- [ ] On GO, **delete the disposable probe** before committing, then commit. The
  probe must not appear in the candidate commit's tree.

```powershell
rustfmt src/compile/fragment_synth/topology.rs src/compile/fragment_synth/instance_graph.rs src/compile/primitive_graph.rs
Invoke-Capped -Command 'cargo test --lib disposable_merge_consumer_index_probe'
```

Expected: `0 tests run` -- the probe is gone.

```powershell
git add -- src/compile/fragment_synth/topology.rs src/compile/fragment_synth/instance_graph.rs src/compile/primitive_graph.rs docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "perf: build the merge consumer index once per batch"
```

---

## Task 4: Candidate 4 -- filtered second bounded Pull-X round

**Files:**

- Modify: `src/compile/fragment_synth/hierarchy_api.rs:900-1013,1015-1120`.
- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Write failing unit tests in `hierarchy_api.rs`'s test module:

  - `pull_x_round_two_offers_only_moved_sinks_that_still_pull` -- an edge whose
    incumbent sink `dx` is zero is not offered even when
    `block_pull_x_proposal` returns `Some`; an edge whose `dx` is non-zero but
    whose proposal is `None` is not offered either; only the conjunction is.
  - `pull_x_round_two_sits_between_round_one_and_the_seam_stage` -- exact stage
    ranges computed from `edges.len()`, frozen round-1 length, frozen round-2
    length, `seams.len()` and the frozen prune length, proving round 2 is
    immediately after round 1 and before seam/prune/refresh.
  - `pull_x_round_two_freezes_once_and_refuses_a_stale_descriptor` -- the
    freeze runs on first reach only; a descriptor whose ports now share an X is
    refused, not retargeted.
  - `pull_x_round_two_carries_lexicographic_acceptance_and_its_own_schemas` --
    the evaluation carries `Acceptance::Lexicographic` and the fingerprints use
    `hierarchical-block-pull-x2-fragment-v1` and
    `hierarchical-block-pull-x2-choice-v1`; round 1's schemas are unchanged.
  - `budget_zero_never_freezes_pull_x_round_two` -- at budget zero the new
    `Option` field is still `None`.

- [ ] Run RED:

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::hierarchy_api::tests::pull_x_round_two -- --nocapture'
```

Expected RED: the round-2 field, helper and schemas do not exist.

- [ ] Implement the minimum, in the shape the existing stages already use. Add
  one field beside `pull_x_edges`:

```rust
/// Round-2 Pull-X descriptors, frozen from the incumbent when round 1 is
/// exhausted: edges whose sink has already moved and that still offer a
/// legal one-cell pull. `None` until the stage is first reached.
pull_x2_edges: Option<Vec<BlockEdge>>,
```

and one boundary helper beside `pull_x_edge`:

```rust
fn pull_x2_edge(
    &mut self,
    index: usize,
    incumbent: &HierarchicalCandidate,
) -> Option<BlockEdge>;
```

`pull_x2_edge` subtracts `self.edges.len() + pull_x`, where `pull_x` reads the
already-frozen round-1 vector, and its filter is exactly

```rust
incumbent
    .block_placements
    .get(&edge.sink_block)
    .is_some_and(|placement| placement.dx != 0)
    && block_pull_x_proposal(..).is_some()
```

Add one stage arm to `next`, immediately after the round-1 arm, yielding
`Acceptance::Lexicographic` through the existing four-element tuple. The seam,
prune and refresh boundary helpers each gain the frozen round-2 length in their
subtraction, in the same `map_or(0, Vec::len)` shape they already use for
round 1.

- [ ] Run GREEN and prove the existing stages did not move:

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture'
Invoke-Capped -Command 'cargo test --lib block_pull_x'
Invoke-Capped -Command 'cargo test --lib refresh_relocation'
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::search -- --nocapture'
```

- [ ] Cheap pre-gate, **plan-only**. Add one ignored test
  `pull_x_round_two_offers_descriptors_on_ripple_adder8` that builds the same
  private stream `compile_hierarchical` builds, runs it until round 1 is
  exhausted, freezes round 2, and prints the frozen length. This is plan-only by
  construction: it reads placements, not routes, because the route structure a
  real gain depends on does not exist until the proposal is replanned. It cannot
  predict a win and the report must say so.

```powershell
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::pull_x_round_two_offers_descriptors_on_ripple_adder8 -- --ignored --exact --nocapture'
```

If the frozen length is **0**, the candidate is NO-GO immediately: revert and do
not spend the certified run.

- [ ] Retention: **at most one** capped certified run, on `ripple_adder8` only.
  Add one ignored test `pull_x_round_two_improves_ripple_adder8` that runs that
  one circuit's stream through `run_budgeted_proposals` and asserts at least one
  accepted trace entry lies inside the round-2 stage range with a strictly better
  `QualityKey` than the entry before it. This is Candidate 4's whole attribution
  budget; Task 6 does not repeat it and never extends it to the other circuits.

```powershell
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::pull_x_round_two_improves_ripple_adder8 -- --ignored --exact --nocapture'
```

A capped run is a failed measurement and therefore NO-GO. Do not re-run it with
a different budget to look for a better answer; one run is the whole allowance.

- [ ] On NO-GO, remove the stage, both ignored tests and the unit tests:

```powershell
git checkout -- src/compile/fragment_synth/hierarchy_api.rs
git status --porcelain
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture'
```

- [ ] On GO, keep the unit tests and the retention test, delete the plan-only
  pre-gate test (it has served its purpose and predicts nothing), and commit.

```powershell
rustfmt src/compile/fragment_synth/hierarchy_api.rs
git add -- src/compile/fragment_synth/hierarchy_api.rs docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "feat: offer a second bounded pull-x round"
```

---

## Task 5: Candidate 5 -- insert-then-prune fallback for Refresh Relocation

This subsumes bend-aware relocation and sibling merge. They are one route
rewrite and get no stage of their own.

**Files:**

- Modify: `src/compile/fragment_synth/route_opt.rs:96-281,465-590`.
- Modify: `src/compile/fragment_synth/hierarchy_api.rs` test module near
  `:3300-3400`.
- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Build `bent_relocation_route()` in the existing test module from
  `linear_relocation_route` plus the existing `divert` / `remap_anchors` /
  `onto_z_axis` / `step_by` helpers, so the trunk turns once between the two
  standing refreshes. Do not add a second fixture builder and do not assign
  `BlockState::facing` directly; use `crate::compile::repeater(Facing::..)` as
  the existing helpers do.

- [ ] Write RED tests in `route_opt.rs`:

  - `refresh_insert_fallback_breaks_a_bend_that_prune_and_relocation_both_refuse`
    -- on `bent_relocation_route()`, `prune_route` alone changes nothing and the
    straight relocation alone changes nothing, but `relocate_refresh` now
    retains a step whose repeater anchors show exactly one fewer repeater.
  - `refresh_insert_fallback_tries_only_the_furthest_downstream_cell` -- a
    fixture with several legal insertion cells, where the furthest-downstream one
    fails and an earlier one would have succeeded, proves the pass tries only the
    furthest-downstream cell, restores, and moves on to the **next standing
    refresh** rather than to a second cell.
  - `refresh_insert_fallback_validates_every_branch_through_the_inserted_cell`
    -- a branch that passes through the inserted cell and dies after it refuses
    the whole fallback; a branch that does not contain the cell is unaffected.
  - `refresh_insert_fallback_requires_a_strict_repeater_reduction` -- inserting
    one and pruning exactly one is no gain and is undone.
  - `refresh_insert_fallback_restores_every_touched_cell_on_no_gain` -- after a
    refused fallback the tree is byte-equal to its input, including every state
    the interleaved `prune_route` changed.
  - `refresh_insert_fallback_is_order_stable_under_shuffled_cells` -- shuffling
    `tree.cells` and `tree.branches` input order yields the identical result,
    proving nothing reads map or vector iteration order.

- [ ] Run RED:

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::route_opt::tests::refresh_insert_fallback -- --nocapture'
```

Expected RED: the fallback does not exist; `relocate_refresh` leaves the bent
fixture unchanged. Record the exact failure text.

- [ ] Implement inside `relocate_refresh`, reusing what is there. Keep the
  existing straight path exactly as it is and reach the fallback **only** when
  that path retained nothing for the standing refresh. Per standing refresh:

  1. enumerate route-owned dust cells downstream of the refresh in
     `Reverse(maximum path depth)` then `Anchor` order, skipping terminals;
  2. take the **furthest-downstream** cell, the first in that order, and only
     that one;
  3. snapshot the full cell state of the whole tree (`tree.cells.clone()`);
  4. write the standing refresh's own state, with the proven successor facing,
     onto that cell;
  5. require `branches_carry_through` to hold for **every branch containing the
     inserted cell**;
  6. run the existing `prune_route` on the mutated tree;
  7. retain only if the route's repeater count **strictly falls**; otherwise
     restore the snapshot exactly and continue to the **next standing refresh**.
     Never try a second cell for the same refresh.

  No new module, no relocation framework, no second search, no work cap knob.

- [ ] Run GREEN plus every existing route and union regression:

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::route_opt -- --nocapture'
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::union -- --nocapture'
Invoke-Capped -Command 'cargo test --lib refresh_relocation'
```

`cargo test --lib refresh_relocation` must still report **9 passed / 2 ignored**
plus the new tests; no previously passing refusal test may be weakened to make
the fallback fit.

- [ ] Retention on a real circuit: **one** targeted attribution run, the whole
  budget for this candidate. Extend the existing ignored
  `refresh_relocation_improves_an_acceptance_circuit` rather than adding a second
  real-circuit runner: assert at least one accepted refresh-stage trace entry
  whose gain comes from the fallback, with `observed_settle` strictly lower and
  `non_air_blocks`, `occupied_volume` and `static_routed_delay` each no worse --
  the unchanged `JointQuality` guard. Task 6 does not repeat it.

```powershell
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::refresh_relocation_improves_an_acceptance_circuit -- --ignored --exact --nocapture'
```

- [ ] On NO-GO -- no accepted fallback-attributed entry on any acceptance
  circuit -- remove the fallback, the fixture and every new test:

```powershell
git checkout -- src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/hierarchy_api.rs
git status --porcelain
Invoke-Capped -Command 'cargo test --lib refresh_relocation'
```

- [ ] On GO, commit.

```powershell
rustfmt src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/hierarchy_api.rs
git add -- src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/hierarchy_api.rs docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "feat: insert one legal repeater when relocation is refused"
```

---

## Task 6: One bounded final validation, over the retained set only

Run this **once**, after Tasks 1-5 have each been retained or removed. If zero
candidates were retained, skip to Task 7 and say so: there is nothing to
validate.

This task stays low-cost by construction. **Exhaustion is never run here.**
`multiplier4` at `u64::MAX` is measured at over 68 minutes, and a four-circuit
exhaustion run is how the previous cycle turned sub-10-minute commands into a
multi-hour one. Candidates 4 and 5 already spent their one targeted certified
attribution run each in Tasks 4 and 5; those are not repeated.

**Files:**

- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Confirm the tree is clean and record the validated revision.

```powershell
git status --porcelain
git rev-parse HEAD
```

- [ ] Four-circuit semantic check at **budget 0**, one case per capped command.
  This proves the seed still certifies identically on every acceptance circuit;
  it is not an optimization run and must not be given a nonzero budget.

```powershell
foreach ($case in 'ripple_adder8','alu4_full','multiplier4','alu8') {
    $log = Join-Path $env:TEMP "reda-wave-validate-$case.txt"
    $cmd = @"
`$env:REDA_EXTRA_CIRCUITS='$case'
`$env:REDA_RETENTION_BUDGET='0'
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture
"@
    Invoke-Capped -Command $cmd -Log $log
}
```

- [ ] Pinned IO, once.

```powershell
Invoke-Capped -Command 'cargo test --test build_circuit_pins -- --nocapture'
```

- [ ] Worker 1/2/4 determinism. The existing matrix already compiles at
  `SynthesisBudget::Evaluations(1)`; keep it there. Run **one selected real
  circuit per capped command**. Run all four in one command only if a measured
  run shows it finishes inside the cap, and record that measurement.

```powershell
foreach ($case in 'ripple_adder8','alu4_full') {
    $cmd = @"
`$env:REDA_EXTRA_CIRCUITS='$case'
cargo test --release --lib compile::fragment_synth::seed::tests::every_hierarchical_circuit_agrees_across_certification_thread_counts -- --ignored --exact --nocapture --test-threads=1
"@
    Invoke-Capped -Command $cmd -Log (Join-Path $env:TEMP "reda-wave-threads-$case.txt")
}
```

- [ ] **Flat control: deferred, on the record.** `fragment_baseline` +
  `fragment_acceptance` is measured at roughly 40 minutes and does not fit a
  600 s cap; wrapping it in `Invoke-Capped` would only guarantee a capped, failed
  measurement. Do not pretend otherwise and do not raise the cap for it. Either:

  - run an **existing bounded flat control** instead -- the `--lib`
    `compile::fragment_synth::fragment` and `compile::fragment_synth::search`
    suites, which cover the flat search path the retained candidates touch --
    and say that is what was run; or
  - record it as **deferred** with the reason and the residual risk.

```powershell
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::fragment -- --nocapture'
Invoke-Capped -Command 'cargo test --lib compile::fragment_synth::search -- --nocapture'
```

  The residual risk to write down: no retained candidate was proven against the
  full flat acceptance corpus in this wave. Candidate 1 is the only one that can
  reach the flat path at all, and only through the shared simulator; Candidates
  2-5 are hierarchical-only. Schedule the full flat control as its own
  out-of-band run if Candidate 1 is retained.

- [ ] Record, per retained candidate and for the retained set as a whole: all
  four `QualityKey` fields, `evaluations_used`, `stop_reason`, every trace entry,
  both fingerprints, the ordered `WORK` sequence, the pinned-IO verdict, the
  worker matrix verdict, the flat-control ruling, and the running total against
  the 60-minute wave budget. Any disagreement with the baseline is a **stop**:
  revert the candidate responsible, re-run this task for the remaining set, and
  record both attempts.

- [ ] Commit the validation section alone.

```powershell
git add -- docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "docs: validate the retained low-cost candidates"
```

---

## Task 7: Close the wave

**Files:**

- Modify: `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.

- [ ] Prove nothing was left behind. For each NO-GO candidate, diff `HEAD`
  against the wave start commit and confirm the candidate's files carry no trace
  of it.

```powershell
$waveStart = (git log --format='%H' -1 --grep='^docs: record five-candidate wave baseline').Trim()
if (-not $waveStart) { throw 'wave start commit not found' }
git diff --stat $waveStart..HEAD
Invoke-Capped -Command 'cargo test --lib disposable_merge_consumer_index_probe'
```

Expected: the diff lists only retained candidates' files, the report, and
`hierarchy_api.rs` for Task 2's two `PHASE` lines; the probe reports `0 tests run`.

- [ ] Grep for survivors that the diff would not make obvious, and record the
  output verbatim.

```powershell
Select-String -Path 'src/**/*.rs' -Pattern 'REDA_(?!PHASE_TIMING|EXTRA_CIRCUITS|RETENTION_BUDGET)' -AllMatches
Select-String -Path 'src/**/*.rs' -Pattern 'disposable|probe_counter|TODO|FIXME' -AllMatches
```

Any new match is a survivor and must be removed before this task commits.

- [ ] Write the wave verdict: **retained N of 5**, naming each candidate as
  RETAINED or ATTEMPTED-NO-GO with its measurement, its gate, and the commit that
  landed or removed it. State plainly that the wave was five evaluations and not
  a promise of five passes.

- [ ] Run the full cheap gate set one last time and record it beside the Task 0
  baseline, together with the final wave-budget total.

```powershell
Invoke-Capped -Command 'cargo test --release --lib merge_isolation -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1'
Invoke-Capped -Command 'cargo test --lib block_pull_x'
Invoke-Capped -Command 'cargo test --lib refresh_relocation'
Invoke-Capped -Command 'cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan'
```

- [ ] Commit.

```powershell
git add -- docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md
git diff --cached --check
git commit -m "docs: close the five-candidate optimization wave"
```
