# Five Low-Cost Optimization Passes: Wave Report

Spec: `docs/superpowers/specs/2026-09-11-five-low-cost-optimization-passes.md`.
Plan: `docs/superpowers/plans/2026-09-11-five-low-cost-optimization-passes.md`.
This report is append-only: each task adds its section below the previous one.

## Task 0: baseline verification and wave start

Date: 2026-09-11. Docs HEAD when Task 0 started: `cda5075012075634db567d9a0c767ddd02707628`.

### Provenance

| Check | Result |
| --- | --- |
| Branch | `claude/topology-aware-seed-v2-6f8f7e` |
| Worktree | clean (`git status --porcelain` empty) |
| Source baseline `6c9f8b50cb00759a07c28f839c9180dac69696aa` is an ancestor of HEAD | yes |
| `git diff --exit-code 6c9f8b5 HEAD -- src` | empty; `src` is byte-identical to the baseline |
| Toolchain | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, PowerShell 7.1.3 |

### The capped runner used

Every Cargo command in this task ran through this definition, one at a time,
exactly as written in the plan:

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
        Invoke-Expression $c 2>&1
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

`Read-RippleSample` is the plan's definition verbatim.

**Runner finding (binding for every later ripple measurement).** With the
`$ripple` literal exactly as written in the plan, the job's transcript carries
only the child's **stdout**: the `RETENTION` lines (which are `println!`) arrive,
but every `PHASE` and `WORK` line (which are `eprintln!`) is lost, and
`Read-RippleSample` returns `TopManifestMs = $null` and an empty `Work`. The
`2>&1` on `Invoke-Expression` inside `Start-Job` does not merge a native
child's stderr. This was reproduced on PowerShell 7.1.3 and Windows PowerShell
5.1.19041 with `rustc --bogus-flag` inside the same job shape. Placing `2>&1`
on the cargo line **inside the command string** does merge it. The runner was
not changed; the ripple command string was. The exact string used for the
paired samples below, and which Candidates 1 and 4 must reuse unchanged, is:

```powershell
$ripple = @'
$env:REDA_EXTRA_CIRCUITS='ripple_adder8'
$env:REDA_RETENTION_BUDGET='0'
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture 2>&1
'@
```

The merged transcript contains exactly one artifact line,
`System.Management.Automation.RemoteException`, standing in for the first
stderr line; every `PHASE name millis` line parses with the plan's anchored
patterns. The two stdout-only commands run first are real capped command time
and are counted in the budget ledger below; their transcripts are kept as
`reda-wave-baseline-sample2-stdout-only.txt` and `...sample3-stdout-only.txt`.

### Ripple budget-0 paired samples

Three paired samples now exist. Sample 1 is the value recorded in the spec at
`6c9f8b5`; samples 2 and 3 are the merged-stderr commands above. Each command
compiles `ripple_adder8` twice; one command is one paired sample read as the
maximum top `PHASE manifest` and the maximum `RETENTION` `wall_ms`.

| Paired sample | Top `PHASE manifest` (the pair) | Max `wall_ms` (the pair) | Cap | Transcript |
| --- | --- | --- | --- | --- |
| 1 (recorded) | 3749 (3609, 3749) | 9789 (9536, 9790) | under 600 s | spec baseline |
| 2 | 3571 (3571, 3560) | 9586 (9585, 9586) | 19.8 s | `$env:TEMP\reda-wave-baseline-sample2.txt`, SHA-256 `7890c25b6b9feac0e3e62afb03d1c50211d26a5bbbc655825befa8053b2cf141` |
| 3 | 3637 (3593, 3637) | 9642 (9583, 9642) | 19.8 s | `$env:TEMP\reda-wave-baseline-sample3.txt`, SHA-256 `ffa3b9dadb2c45d40d563f6f27fb4fc8a1fe7769e2c7f6701b114b12ead5fd5f` |
| **Baseline median (3 paired samples)** | **3637 ms** | **9642 ms** | | |

Candidate 1's gate therefore reads: median top `PHASE manifest` must be at most
2424 ms (3637 / 1.5), and median `RETENTION` `wall_ms` must be at most 10124 ms
(9642 x 1.05), with exact quality, `WORK` sequence and fingerprints.

Both merged transcripts also carry the smaller `PHASE manifest` of the child
block compile (30 / 27 ms and 29 / 29 ms); the maximum is the top-level one.

Verbatim assertion, run over samples 2 and 3, all three strings present in
each:

```text
settle=608 blocks=70603 volume=1123332 static=678
b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573
a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2
```

The four `RETENTION` lines across samples 2 and 3 (two per command) all read
`settle=608 blocks=70603 volume=1123332 static=678 evals=0 stop=EvaluationBudget`
with `case=b9ab139a...4573` and `candidate=a5e71ef0...b1b2`. The stdout-only
commands reproduced the same four fields and both fingerprints as well.

Ordered `WORK` sequence per command (14 lines, identical in samples 2 and 3;
this is the baseline sequence every retained candidate must reproduce in order):

```text
WORK exhaustive_workers 1
WORK exhaustive_vectors 8
WORK manifest_workers 8
WORK manifest_transitions 56
WORK exhaustive_vectors 0
WORK manifest_workers 12
WORK manifest_transitions 68
WORK exhaustive_workers 1
WORK exhaustive_vectors 8
WORK manifest_workers 8
WORK manifest_transitions 56
WORK exhaustive_vectors 0
WORK manifest_workers 12
WORK manifest_transitions 68
```

### Focused gates (one capped command each)

| Gate | Command | Result | libtest time | Command elapsed | Recorded at `6c9f8b5` |
| --- | --- | --- | --- | --- | --- |
| Merge correctness (1 of 2) | `cargo test --release --lib merge_isolation -- --nocapture --test-threads=1` | 1 passed | 0.00 s | 0.6 s | 18 tests total across both commands, 0.277 s |
| Merge correctness (2 of 2) | `cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1` | 17 passed | 0.00 s | 0.6 s | (as above) |
| Pull-X | `cargo test --lib block_pull_x` | 2 passed, 1 ignored | 3.92 s | 29.7 s (includes the debug build) | 2 passed / 1 ignored, 0.672 s |
| Refresh | `cargo test --lib refresh_relocation` | 9 passed, 2 ignored | 3.02 s | 3.6 s | 9 passed / 2 ignored, 0.728 s |
| Reuse fixture | `cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan` | 1 passed | 20.75 s | 21.4 s | 1.98 s warm, 75.197 s cold command |

Merge correctness totals 18 tests across its two commands, as recorded. The
pass/ignore counts match the recorded values exactly. The libtest durations for
the debug-profile gates are slower than the recorded ones (Pull-X 3.92 s vs
0.672 s, refresh 3.02 s vs 0.728 s, reuse 20.75 s vs 1.98 s); pass/fail counts
are the gate for these three, and the reuse fixture's aggregate `PHASE prunable`
cost, not its libtest duration, is Candidate 2's measured quantity. Candidate 2
must therefore also carry `2>&1` inside its command string or it will read no
`PHASE prunable` lines at all.

Transcripts: `$env:TEMP\reda-task0-merge-isolation.txt`,
`reda-task0-instance-graph-tests.txt`, `reda-task0-pull-x.txt`,
`reda-task0-refresh-relocation.txt`, `reda-task0-reuse-fixture.txt`.

### Baseline summary table

| Signal | Value |
| --- | --- |
| Worktree / branch | clean, `claude/topology-aware-seed-v2-6f8f7e` |
| Source baseline | `6c9f8b50cb00759a07c28f839c9180dac69696aa` |
| Ripple budget-0 top `PHASE manifest`, three paired samples | 3749, 3571, 3637; median 3637 ms |
| Ripple budget-0 `RETENTION` `wall_ms`, three paired samples | 9789, 9586, 9642; median 9642 ms |
| Ripple budget-0 quality | settle 608, blocks 70603, volume 1123332, static 678 |
| Ripple budget-0 case fingerprint | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` |
| Ripple budget-0 candidate fingerprint | `a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2` |
| Merge correctness gate | 1 + 17 = 18 passed |
| Pull-X focused gate | 2 passed / 1 ignored |
| Refresh focused gate | 9 passed / 2 ignored |
| Reuse fixture | 1 passed, 20.75 s libtest, 21.4 s command |

### Wave command-time ledger

Every `Invoke-Capped` command, in execution order, with its measured elapsed
time. No command reached the 600 s cap.

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 1 | ripple sample 2, plan literal (stdout-only transcript) | 22.3 s | 22.3 s |
| 2 | ripple sample 3, plan literal (stdout-only transcript) | 20.2 s | 42.5 s |
| 3 | merge_isolation (release) | 0.6 s | 43.1 s |
| 4 | instance_graph::tests (release) | 0.6 s | 43.7 s |
| 5 | block_pull_x | 29.7 s | 73.4 s |
| 6 | refresh_relocation | 3.6 s | 77.0 s |
| 7 | unchanged_block_placements_reuse_the_incumbent_plan | 21.4 s | 98.4 s |
| 8 | ripple sample 2, merged stderr | 19.8 s | 118.2 s |
| 9 | ripple sample 3, merged stderr | 19.8 s | 138.0 s |

**Used after Task 0: 138.0 s = 2.3 min of the 60-minute wave budget.**

### Candidates

| Candidate | Status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | PENDING |
| 2: `prunable_parent_routes` sidecar | PENDING |
| 3: hoisted merge consumer index | PENDING |
| 4: filtered second bounded Pull-X round | PENDING |
| 5: one-shot bend-aware Refresh Relocation | PENDING |

**Wave verdict: retained 0 of 5 so far.**

### Wave start commit

The wave start commit is the commit that adds this file, titled
`docs: record five-candidate wave baseline`; Task 1's revert target is that
commit, and every later candidate's revert target is the commit standing when
that candidate starts. Its SHA cannot be written into its own commit; it is
recorded in the Task 0 implementer report and in the controller ledger, and
Task 1 copies it here as the first line of its section.

## Task 0 correction: native stderr is captured by the runner itself

Controller ruling on the runner finding above: the defect is real and
load-bearing, so it is fixed centrally in the plan rather than by asking every
caller to append `2>&1`. Wave start commit: `c737335494687abfbfb22f714a350bca609f60ea`.

### The central fix

`Invoke-Capped` in the plan now wraps the parsed command in a script block and
redirects that block, inside the job:

```powershell
        Invoke-Expression "& {`n$c`n} 2>&1"
```

replaces `Invoke-Expression $c 2>&1`. Nothing else in the runner changed: the
`REDA_CAPPED_EXIT` sentinel, the nonzero-exit throw, the 600 s `Wait-Job`
timeout and its `Stop-Job` / `Remove-Job` cleanup are byte-identical. The
`$ripple` and `$reuse` literals in the plan carry no `2>&1`; the runner owns
the stderr contract, and the `$ripple` string shown earlier in this report with
a trailing `2>&1` is superseded by the plan's literal for every later task.

### Validation without Cargo

The function text was extracted verbatim from the edited plan file
(`awk` from `function Invoke-Capped` to its closing brace) and dot-sourced, then
run against a multi-line native command that sets an environment variable,
writes stdout, writes stderr and exits nonzero:

```powershell
$probe = @'
$env:REDA_PROBE='1'
rustc --version
rustc --bogus-flag
'@
Invoke-Capped -Command $probe -Log $log
```

Result on PowerShell 7.1.3 and on Windows PowerShell 5.1.19041, identical:

```text
THREW: exit 1 : <the command text>
LOG:
  | rustc 1.97.1 (8bab26f4f 2026-07-14)
  | error: Unrecognized option: 'bogus-flag'
  | System.Management.Automation.RemoteException
SUCCESS PATH RETURNED: rustc 1.97.1 (8bab26f4f 2026-07-14)
REDA_PROBE in interactive shell: []
```

The stderr line is in the log, the nonzero exit throws through the sentinel,
the zero-exit path (`rustc --version`) returns the body, and the environment
variable set inside the command does not leak into the interactive shell. The
single `System.Management.Automation.RemoteException` artifact line stands in
for the blank line the child emits after its error and does not match any
anchored `^PHASE …$`, `^WORK ` or `^RETENTION ` pattern. The 600 s timeout was
not exercised (no 600 s sleep); its code is unchanged by inspection of the diff.

### Why the already-captured baseline stands

Samples 2 and 3 above were captured with `2>&1` on the cargo line inside the
command string. Redirecting the native command's stderr at that line and
redirecting the enclosing script block are behaviourally equivalent: both merge
the same stream into the same job output, and the earlier transcripts show the
same artifact line and the same parseable `PHASE` / `WORK` / `RETENTION`
lines the central form produces. The `RETENTION` lines, quality fields,
fingerprints and `WORK` sequence come from stdout in both forms. The three
paired samples, their medians (3637 ms / 9642 ms) and the focused-gate results
are therefore valid baseline evidence and are not recaptured. The budget ledger
is unchanged at 138.0 s: this correction ran no Cargo command.

## Task 1: Candidate 1 -- palette-indexed `BlockFlags` memo -- attempted, NO-GO

Wave start commit: `c737335494687abfbfb22f714a350bca609f60ea`. Task 1 start and
revert target: `1b1c0ac7dfe1c8b0d7ba1ff92714052b7954893f`.

Branch taken: **NO-GO**. The memo is correct and semantically exact, but the
measured top `PHASE manifest` speedup is **1.18x**, short of the required 1.5x.
Every source change was removed before this section was committed.

### TDD evidence

Six behavioural tests were written first; every assertion compares values the
public API returns, and none inspects source text or field layout.

| Test | Module | Status before production code |
| --- | --- | --- |
| `flags_are_interned_alongside_every_state` | `palette.rs` | RED (did not compile) |
| `flags_of_an_unknown_index_is_none` | `palette.rs` | RED (did not compile) |
| `flags_at_agrees_with_flags_of_on_every_placed_cell` | `storage.rs` | RED (did not compile) |
| `flags_at_out_of_bounds_equals_the_in_bounds_air_cell` | `storage.rs` | RED (did not compile) |
| `flags_at_sees_a_palette_entry_interned_after_construction` | `storage.rs` | RED (did not compile) |
| `from_parts_answers_flags_at_for_a_palette_that_had_no_air` | `storage.rs` | RED (did not compile) |

**RED**, `cargo test --lib redstone::world -- --nocapture`, exit 101, 5.3 s.
Exact failure text (first and last of the nine errors, plus the summary):

```text
error[E0599]: no method named `flags` found for struct `palette::Palette` in the current scope
   --> src\redstone\world\palette.rs:106:19
    |
 12 | pub struct Palette {
    | ------------------ method `flags` not found for this struct
...
106 |                 p.flags(index),

error[E0599]: no method named `flags_at` found for struct `storage::World` in the current scope
   --> src\redstone\world\storage.rs:497:19
    |
 32 | pub struct World {
    | ---------------- method `flags_at` not found for this struct
...
497 |                 w.flags_at(*x, *y, *z),

For more information about this error, try `rustc --explain E0599`.
error: could not compile `reda` (lib test) due to 9 previous errors
```

**GREEN**, after the production change:

| Command | Result | Elapsed |
| --- | --- | --- |
| `cargo test --lib redstone::world -- --nocapture` | 24 passed; 0 failed | 25.2 s |
| `cargo test --lib redstone::simulator -- --nocapture` | 120 passed; 0 failed | 0.8 s |

All six new tests are named individually in the world transcript as `ok`.
`dust_topology_epoch_ignores_dynamic_state_and_tracks_connectivity_flags` is
**characterization**: it was green before this change and stayed green
(`test redstone::world::storage::tests::dust_topology_epoch_ignores_dynamic_state_and_tracks_connectivity_flags ... ok`).

### The candidate as implemented (now removed)

`Palette` owned the memo, exactly as planned: `flags: Vec<BlockFlags>` pushed in
the same `intern` branch that pushes `entries`, plus
`Palette::flags(&self, index: u32) -> Option<BlockFlags>`. `World` gained only
`flags_at`, delegating to the palette with the cell's index in bounds and
`air_index` out of bounds; it held no second copy and no second invariant. No
`debug_assert_eq!` was added -- the four `storage.rs` tests already pin the
equivalence and a per-neighbour assert would have changed what the debug build
measures. `dust_topology_key` and `dust_topology_changed` were untouched.

Exactly three call sites switched, as specified: `connectivity.rs`'s
`is_conductive` and `supports_dust_step`, and the `block_signal_at` conductivity
guard in `propagate.rs`. Every other `flags_of` call site was left alone; the two
`flags_of` imports that this left unused were dropped from their `use` lines.

```text
 src/redstone/simulator/connectivity.rs |  6 +--
 src/redstone/simulator/propagate.rs    |  5 +--
 src/redstone/world/palette.rs          | 51 +++++++++++++++++++++
 src/redstone/world/storage.rs          | 82 +++++++++++++++++++++++++++++++++-
 4 files changed, 137 insertions(+), 7 deletions(-)
```

Of those 137 lines, 97 are the six new tests; the production surface is three
changed call sites, one new field, and two new accessors.

### Retention gate: three ripple budget-0 paired samples

One capped command is one paired sample, read as the maximum top
`PHASE manifest` and the maximum `RETENTION` `wall_ms` within that command. The
`$ripple` literal was the plan's, unmodified; the corrected `Invoke-Capped`
captured native stderr, so every `PHASE` and `WORK` line parsed.

| Paired sample | Top `PHASE manifest` (the pair) | Max `wall_ms` (the pair) | Elapsed | Transcript |
| --- | --- | --- | --- | --- |
| 1 | 3072 (3072, 2916) | 9172 (9172, 9045) | 92.5 s | `$env:TEMP\reda-wave-c1-sample1.txt` |
| 2 | 3034 (3034, 2849) | 9014 (9014, 8946) | 18.6 s | `$env:TEMP\reda-wave-c1-sample2.txt` |
| 3 | 3125 (3125, 2912) | 9162 (9162, 9065) | 18.9 s | `$env:TEMP\reda-wave-c1-sample3.txt` |
| **Candidate median** | **3072 ms** | **9162 ms** | | |

Sample 1 is slower as a command only because it included the release rebuild;
the measured quantities are the harness's own phase and wall numbers, not the
command elapsed time. Each transcript also carries the child block compile's
smaller `PHASE manifest` (25 / 23, 25 / 23, 28 / 26 ms); the maximum is the
top-level one.

### Computed ratios against the baseline

| Gate | Baseline median | Candidate median | Computed | Threshold | Verdict |
| --- | --- | --- | --- | --- | --- |
| Top `PHASE manifest` | 3637 ms | 3072 ms | 3637 / 3072 = **1.18x** | at least 1.5x, i.e. at most 2424 ms | **FAIL** |
| `RETENTION` `wall_ms` | 9642 ms | 9162 ms | 9162 / 9642 = 0.950, i.e. 5.0% **faster** | at most 1.05x, i.e. at most 10124 ms | pass |

### Exact semantic and determinism comparison

Identical to the baseline in every checked respect, across all three samples:

| Artifact | Result |
| --- | --- |
| Quality, all six `RETENTION` lines | `settle=608 blocks=70603 volume=1123332 static=678` |
| Case fingerprint | `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573` |
| Candidate fingerprint | `a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2` |
| Ordered `WORK` sequence | 14 lines per sample, byte-identical to the baseline sequence, compared with `diff` as an ordered sequence |
| `evals` / `stop` | `evals=0 stop=EvaluationBudget`, as at baseline |

The candidate is therefore *correct*; it simply is not fast enough. Per the
wave's standing ruling, the measured phase number decides -- a call-count or
asymptotic argument does not substitute for it, and none is offered here.

### Removal proof

```powershell
git checkout -- src/redstone/world/palette.rs src/redstone/world/storage.rs src/redstone/simulator/connectivity.rs src/redstone/simulator/propagate.rs
git status --porcelain
```

`git status --porcelain` was **empty** immediately after the checkout (this
report section had not yet been written), and
`git diff --exit-code 1b1c0ac7dfe1c8b0d7ba1ff92714052b7954893f -- src` was empty:
`src` is byte-identical to the Task 1 start commit. The focused world suite was
then re-run on the restored tree: **18 passed; 0 failed**, 25.4 s -- the
pre-candidate count, since the six new tests exercised an API that no longer
exists and were removed with it. The only working-tree change at commit time is
this report.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 10 | RED: `cargo test --lib redstone::world` (expected compile failure) | 5.3 s | 143.3 s |
| 11 | GREEN: `cargo test --lib redstone::world` | 25.2 s | 168.5 s |
| 12 | GREEN: `cargo test --lib redstone::simulator` | 0.8 s | 169.3 s |
| 13 | ripple paired sample 1 | 92.5 s | 261.8 s |
| 14 | ripple paired sample 2 | 18.6 s | 280.4 s |
| 15 | ripple paired sample 3 | 18.9 s | 299.3 s |
| 16 | removal proof: `cargo test --lib redstone::world` | 25.4 s | 324.7 s |

**Used after Task 1: 324.7 s = 5.4 min of the 60-minute wave budget.** No
command reached the 600 s cap.

### Candidates

| Candidate | Status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | **attempted, NO-GO** (1.18x vs 1.5x required) |
| 2: `prunable_parent_routes` sidecar | PENDING |
| 3: hoisted merge consumer index | PENDING |
| 4: filtered second bounded Pull-X round | PENDING |
| 5: one-shot bend-aware Refresh Relocation | PENDING |

**Wave verdict: retained 0 of 5 so far.**

---

## Task 2: Candidate 2 -- `prunable_parent_routes` diagnostics, then a conditional sidecar -- attempted, GO

Task 2 start and revert target: `79e62b6`. Diagnostics commit: `cdd48bb`.

Branch taken: the kill switch did **not** fire. The measured median relevant
prunable cost on the plan-reuse fixture is **26 ms**, above the 5 ms threshold,
so Step C was executed; the sidecar halves that cost to **13 ms** and is
**retained**.

### Step A -- diagnostics (not TDD)

Two permanent `PHASE` lines were added inside `union_and_certify`, in the exact
shape of the existing `PHASE union` and under its exact `REDA_PHASE_TIMING`
guard: `PHASE flatten` around `module_flattening` and `PHASE prunable` around
`prunable_parent_routes`.

**These two lines carry no test, and no TDD claim is made for them.** They are
diagnostics; the transcripts below are their evidence. They were committed on
their own, as `cdd48bb`, so the kill-switch decision was made from a clean,
committed revision.

### Step A measurement -- the plan-reuse fixture

Gate fixture: `unchanged_block_placements_reuse_the_incumbent_plan`, which is the
one existing test that drives the plan-reuse branch. Ripple budget 0 is *not*
this candidate's gate: it never reuses a plan.

```powershell
$reuse = @'
$env:REDA_PHASE_TIMING='1'
cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture --test-threads=1
'@
```

Each command compiles the parent four times, so each transcript carries four
`PHASE prunable` lines. The relevant prunable cost of one command is that
command's aggregate.

| Command | `PHASE prunable` lines | **AggregateMs** | `PHASE flatten` lines | Elapsed | Transcript |
| --- | --- | --- | --- | --- | --- |
| 1 | 6, 7, 6, 6 | **25** | 0, 0, 0, 0 | 27.8 s | `$env:TEMP\reda-wave-c2-diag1.txt` |
| 2 | 6, 7, 7, 6 | **26** | 0, 0, 0, 0 | 21.0 s | `$env:TEMP\reda-wave-c2-diag2.txt` |
| 3 | 7, 7, 6, 6 | **26** | 0, 0, 0, 0 | 21.0 s | `$env:TEMP\reda-wave-c2-diag3.txt` |
| **Median** | | **26 ms** | | | |

The fixture passed in all three commands (`1 passed; 0 failed`, ~20.6 s in-test).
`PHASE flatten` is **0 ms every time** on this fixture -- `module_flattening` is
not a cost here, which is exactly what a diagnostic is for and why no flattening
hoist is proposed.

### Step B -- the kill switch did not fire

Median relevant prunable cost **26 ms > 5 ms**, so the sidecar was written.

### Step C TDD evidence

Two tests were written before any production change. Both assert on values the
compile path returns -- `Arc` identity and the derived set -- and neither
inspects source text.

| Test | Status before production code |
| --- | --- |
| `an_unchanged_placement_reuses_the_routed_parent_and_its_prunable_routes` | RED (did not compile) |
| `a_moved_block_builds_a_new_routed_parent_with_its_own_prunable_routes` | RED (did not compile) |

**RED**, `cargo test --lib routed_parent -- --nocapture`, exit 101, 3.2 s:

```text
error[E0609]: no field `prunable_routes` on type `std::sync::Arc<seed::PlannedParent>`
error[E0609]: no field `prunable_routes` on type `std::sync::Arc<seed::PlannedParent>`
error[E0609]: no field `prunable_routes` on type `std::sync::Arc<seed::PlannedParent>`
error[E0609]: no field `planned` on type `std::sync::Arc<seed::PlannedParent>`
error: could not compile `reda` (lib test) due to 4 previous errors
```

That is the expected RED: `RoutedParent` does not exist.

### The candidate as implemented

One private struct beside `plan_parent`, and the sidecar is computed at the one
place a plan is constructed:

```rust
struct RoutedParent {
    planned: PlannedParent,
    prunable_routes: BTreeSet<RouteId>,
}
```

- `HierarchicalCandidate::planned` is now `Arc<RoutedParent>`.
- Both `Arc::new(..)` plan sites became `Arc::new(RoutedParent::new(..))`; the
  reuse branch keeps its `Arc::clone` and now carries the sidecar for free.
- `union_and_certify` reads `&planned.prunable_routes` instead of calling
  `prunable_parent_routes`, and passes `&planned.planned` to `UnionInput`.
- Every field read was updated, found by symbol search rather than by the plan's
  line list: `union_and_certify`'s `block_offsets` and `UnionInput::parent`,
  `refresh_descriptors`, and the two existing test reads in
  `refresh_stage_freezes_relocatable_routes_from_the_incumbent`. After the
  change, a repository-wide search for the two old field paths matches nothing
  outside `planned.planned.*`.
- `prunable_parent_routes` keeps its signature and body; `union_and_certify`
  keeps its call graph. No `ModuleCompileContext`, no flattening hoist, no
  mutable cache, no pointer-key cache, no production counter.
- **The `PHASE prunable` line moved with the computation**, into
  `RoutedParent::new`, under the same guard and with the same name. A compile
  that reuses a plan now emits no `PHASE prunable` line at all, because no
  computation happens. That is the intended effect, and it is the measurement.

### GREEN

`cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture`:
**25 passed; 1 failed; 6 ignored**, 55.6 s in-test, command elapsed 56.1 s. Both
new tests pass, and the existing plan-reuse and refresh tests still pass.

The one failure is `lowering_an_already_lowered_netlist_is_the_identity`, which
is **pre-existing and environmental**, not caused by this change: it shells out
to yosys and the local Python toolchain refuses to load it
(`RuntimeError: unsupported architecture for wasmtime:`). This was verified, not
assumed: the file was checked back out at the Step A commit `cdd48bb` and the
single test re-run there, where it fails identically (`0 passed; 1 failed`,
23.4 s, same yosys error). The worktree was then restored.

### Retention gate: the three reuse-fixture commands, re-run

| Command | `PHASE prunable` lines | **AggregateMs** | Elapsed | Transcript |
| --- | --- | --- | --- | --- |
| 1 | 6, 7 | **13** | 43.5 s | `$env:TEMP\reda-wave-c2-post1.txt` |
| 2 | 6, 7 | **13** | 21.0 s | `$env:TEMP\reda-wave-c2-post2.txt` |
| 3 | 6, 7 | **13** | 20.9 s | `$env:TEMP\reda-wave-c2-post3.txt` |
| **Median** | | **13 ms** | | |

| Gate | Before | After | Verdict |
| --- | --- | --- | --- |
| Median relevant prunable cost | 26 ms | 13 ms | measurable drop, **13 ms / 50% removed** -- **GO** |
| Fixture assertions | `1 passed; 0 failed` | `1 passed; 0 failed` | pass |

Four lines became two: of the fixture's four parent compiles, two really move a
block and still compute their prunable routes, and the two that reuse the
incumbent's plan now compute nothing and emit no line. The drop is the whole
computation the reuse path used to repeat, and it is the upper bound Step A
predicted. Per the wave's standing ruling, this is a direct measurement, not a
call-count argument.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 17 | Step A reuse-fixture diagnostic 1 | 27.8 s | 352.5 s |
| 18 | Step A reuse-fixture diagnostic 2 | 21.0 s | 373.5 s |
| 19 | Step A reuse-fixture diagnostic 3 | 21.0 s | 394.5 s |
| 20 | RED: `cargo test --lib routed_parent` (expected compile failure) | 3.2 s | 397.7 s |
| 21 | GREEN: `cargo test --lib compile::fragment_synth::hierarchy_api` | not captured, floor 55.9 s | 453.6 s |
| 22 | pre-existing-failure check at `cdd48bb` | 23.4 s | 477.0 s |
| 23 | post-change reuse-fixture 1 | 43.5 s | 520.5 s |
| 24 | post-change reuse-fixture 2 | 21.0 s | 541.5 s |
| 25 | post-change reuse-fixture 3 | 20.9 s | 562.4 s |
| 26 | GREEN re-run (ledger repair, same suite and same result) | 56.1 s | 618.5 s |

Command 21's wall time was **not captured**: the measuring script stopped at the
runner's nonzero-exit throw before its stopwatch was read. Rather than invent a
number, it is counted at its libtest in-test floor of 55.9 s, and command 26
re-ran the identical suite to obtain a true elapsed (56.1 s, identical result).
The wave total is therefore a slight **under**estimate, by the build time
included in command 21 only.

**Used after Task 2: 618.5 s = 10.3 min of the 60-minute wave budget.** No
command reached the 600 s cap.

### Candidates

| Candidate | Status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | **attempted, NO-GO** (1.18x vs 1.5x required) |
| 2: `prunable_parent_routes` sidecar | **attempted, GO, RETAINED** (26 ms to 13 ms median) |
| 3: hoisted merge consumer index | PENDING |
| 4: filtered second bounded Pull-X round | PENDING |
| 5: one-shot bend-aware Refresh Relocation | PENDING |

**Wave verdict: retained 1 of 5 so far.**

---

## Task 3: Candidate 3 -- hoisted merge consumer index -- attempted, NO-GO before production

Task 3 start and revert target: `0a6b138`.

Branch taken: **NO-GO before production**. The approved disposable probe's
representative premise is false: the real seven-segment netlist has **84 gates
and 0 merge gates**, so neither measured batch calls `merge_isolation_mask` at
all. All three baseline runs measured only **4 ms** for 20
`InstanceGraph::with_variants` repeats and **1 ms** for 20
`primitive_graph::expand` repeats. A target with no merge calls and 5 ms total
work cannot demonstrate the required absolute saving of at least 100 ms.

Controller ruling after those three measurements: apply Ponytail/YAGNI,
immediately reject the candidate, write no production helper and no TDD test,
remove the probe, and preserve only the report. Therefore no RED/GREEN claim is
made for Task 3.

### Disposable baseline probe

The probe used the required real
`crate::circuits::seven_segment::build_seven_segment_netlist()` and
`Library::default_library()`. Each command ran the two batches 20 times through
the current serial `Invoke-Capped` runner.

The `PROBE` and `test result` outputs below are transcript-backed. The
`Invoke-Capped` elapsed values **0.655 / 0.584 / 0.588 / 72.422 / 0.637 s** and
the derived **74.886 / 693.386 s** aggregates are controller-observed metadata
that was not embedded in the transcript files. The transcript hashes therefore
do not independently substantiate those elapsed values or the cap result.

| Baseline run | Netlist | `InstanceGraph::with_variants`, 20 repeats | `primitive_graph::expand`, 20 repeats | Command elapsed | Cap observation | Transcript |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 84 gates, 0 merges | 4 ms | 1 ms | 0.655 s | controller observed under 600 s | `$env:TEMP\reda-wave-c3-baseline1.txt`, SHA-256 `610e8ed43b56520f350fde3f6f893c7bbb5fee60917cdc6888141b3e7c1eb94b` |
| 2 | 84 gates, 0 merges | 4 ms | 1 ms | 0.584 s | controller observed under 600 s | `$env:TEMP\reda-wave-c3-baseline2.txt`, SHA-256 `f1641d7226c959dd599e970706ce3f773058f7311611100a1ab75da6076d25d1` |
| 3 | 84 gates, 0 merges | 4 ms | 1 ms | 0.588 s | controller observed under 600 s | `$env:TEMP\reda-wave-c3-baseline3.txt`, SHA-256 `f1641d7226c959dd599e970706ce3f773058f7311611100a1ab75da6076d25d1` |

The median is therefore 4 ms plus 1 ms. No post-change samples exist because
there was intentionally no production change.

### Removal and correctness proof

The disposable probe was removed with `apply_patch`. Afterwards:

```text
git diff --exit-code 0a6b138 -- src        -> exit 0
rg disposable/helper Task 3 symbols src   -> 0 matches
```

Thus `src` is byte-identical to the Task 3 start commit, including the retained
Task 2 implementation already present at `0a6b138`.

Only the two required existing merge correctness commands were run, in order,
through `Invoke-Capped`:

| Command | Result | Command elapsed | Cap observation |
| --- | --- | --- | --- |
| `cargo test --release --lib merge_isolation -- --nocapture --test-threads=1` | 1 passed; 0 failed | 72.422 s, including release rebuild after probe removal | controller observed under 600 s |
| `cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1` | 17 passed; 0 failed | 0.637 s | controller observed under 600 s |

The exact correctness gate totals **18 passed, 0 failed**.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 27 | Candidate 3 disposable baseline 1 | 0.655 s | 619.155 s |
| 28 | Candidate 3 disposable baseline 2 | 0.584 s | 619.739 s |
| 29 | Candidate 3 disposable baseline 3 | 0.588 s | 620.327 s |
| 30 | merge correctness 1 of 2, release rebuild | 72.422 s | 692.749 s |
| 31 | merge correctness 2 of 2 | 0.637 s | 693.386 s |

**Used after Task 3: 693.386 s = 11.6 min of the 60-minute wave budget.**
The controller observed no command reach the 600 s cap; that observation is not
preserved inside the transcript files. Task 3 used 74.886 s total by the same
controller-observed elapsed metadata.

### Candidates

| Candidate | Status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | **attempted, NO-GO** (1.18x vs 1.5x required) |
| 2: `prunable_parent_routes` sidecar | **attempted, GO, RETAINED** (26 ms to 13 ms median) |
| 3: hoisted merge consumer index | **attempted, NO-GO before production** (approved real probe had 0 merge gates and only 5 ms total work) |
| 4: filtered second bounded Pull-X round | PENDING |
| 5: one-shot bend-aware Refresh Relocation | PENDING |

**Wave verdict: retained 1 of 5 so far.**

---

## Task 4: Candidate 4 -- filtered second bounded Pull-X round -- attempted, GO

Task 4 start commit: `fa3b614`.

Branch taken: **GO, retained**. The one plan-only descriptor pre-gate found
seven eligible round-2 descriptors on `ripple_adder8`, so the one permitted
certified attribution run was spent. That run evaluated the exact round-2
range `14..21`: all seven entries were accepted, and the incumbent improved
from `QualityKey { observed_settle: 576, non_air_blocks: 71133,
occupied_volume: 1401988, static_routed_delay: ExactDelay(678) }` to
`QualityKey { observed_settle: 576, non_air_blocks: 71119, occupied_volume:
1401988, static_routed_delay: ExactDelay(678) }`. The retained winner's
candidate fingerprint is
`da554e7386d5b6e0ce2babb36c5e37431f3864eca59447d9c1f4f94772cf588c`.

### TDD evidence

The five focused tests were written before production code. The required RED
command was:

```text
cargo test --lib compile::fragment_synth::hierarchy_api::tests::pull_x_round_two -- --nocapture
```

It exited 101 after 3.858 s wrapper elapsed with eleven expected compiler
errors: `E0599` for the absent `pull_x2_edge` method and `E0609` for the absent
`pull_x2_edges` field. No production code existed at that point. Transcript:
`$env:TEMP\reda-task4-red.log`, SHA-256
`352ef78eec70c5b532ae9ac6a687fc85269b02d4902c5e87b6a21cb495248ad2`.

The retained unit tests prove:

- only an already-X-moved sink with a still-legal one-cell pull enters round 2;
- exact alignment, round-1, round-2, seam, prune and refresh ranges;
- one-time freezing and refusal, rather than retargeting, of a stale descriptor;
- `Acceptance::Lexicographic`, distinct round-2 schemas, and unchanged round-1
  schemas;
- evaluation budget zero leaves both Pull-X vectors unfrozen.

### Retained implementation

`HierarchicalProposalStream` gained only one field, `pull_x2_edges:
Option<Vec<BlockEdge>>`, one `pull_x2_edge` helper, and one `next` arm. The
helper freezes after round 1 and filters with the reviewed conjunction:
the incumbent sink placement has nonzero `dx` and `block_pull_x_proposal`
still returns `Some`. The arm reuses the existing Pull-X proposal path and
four-element compile tuple, keeps lexicographic acceptance, and uses
`hierarchical-block-pull-x2-fragment-v1` /
`hierarchical-block-pull-x2-choice-v1`. Seam, prune and refresh offsets each
include the frozen round-2 length.

Task 2 is structurally untouched: `HierarchicalCandidate::planned` remains
`Arc<RoutedParent>`, the reuse branch remains `Arc::clone`, and the guarded
`PHASE prunable`, `PHASE flatten` and `PHASE union` diagnostics remain in place.
No dependency, flag, counter, timer, fixture-name production branch or new
abstraction was added.

### Focused GREEN gates

All Cargo commands ran serially through the centrally corrected
`Invoke-Capped`; callers did not append `2>&1`. The output/result columns and
transcript hashes below are transcript-backed. Wrapper elapsed is
controller-observed metadata outside the transcript files.

| Command | Result | Libtest time | Wrapper elapsed | Transcript |
| --- | --- | --- | --- | --- |
| `cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture` | 31 passed; 0 failed; 6 ignored | 69.44 s | not captured; ledger floor 69.440 s | `$env:TEMP\reda-task4-green-hierarchy.log`, SHA-256 `b9f7b7f840267d2e17d04b4e6e0957d8f1522f314b5db82e39dae3f232378d3c` |
| `cargo test --lib block_pull_x` | 2 passed; 0 failed; 1 ignored | 3.83 s | 4.510 s | `$env:TEMP\reda-task4-green-block-pull-x.log`, SHA-256 `3348ee0e6b1f0512d40d028eb10bfae28dcc84c7c90d25387a597b3c1190bf79` |
| `cargo test --lib refresh_relocation` | 9 passed; 0 failed; 2 ignored | 3.05 s | 3.696 s | `$env:TEMP\reda-task4-green-refresh-relocation.log`, SHA-256 `5af0a7c6277e5a360f0f41b90030b256b94b1c635770544d547e92de595596fe` |
| `cargo test --lib compile::fragment_synth::search -- --nocapture` | 11 passed; 0 failed | 0.00 s | 0.605 s | `$env:TEMP\reda-task4-green-search.log`, SHA-256 `341c6f4a777dd96f8a5be66b0bb5b15c11ea613c301307cbc1112686d6485791` |
| final `cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture` after pre-gate removal and `rustfmt` | 31 passed; 0 failed; 7 ignored | 67.24 s | 80.717 s | `$env:TEMP\reda-task4-final-hierarchy.log`, SHA-256 `6ceb5fea61be5d95519b12a194176254a192afb7cd9e50177194c9c239a914cb` |

The only warning is the pre-existing unused `BlockFacts::delay_ticks` field.
The first hierarchy command's wrapper stopwatch output was lost when the
controller detached after 30 seconds. Its transcript independently records a
22.77 s debug rebuild and 69.44 s libtest time, but neither is the wrapper
elapsed. As in Task 2's identical accounting repair, the ledger counts only
the 69.440 s libtest floor and preserves the later exact-wrapper rerun; the
wave total is therefore an underestimate by that first command's build and
wrapper overhead.

### Descriptor pre-gate and certified attribution

The plan-only pre-gate ran exactly once:

```text
PULL_X2_DESCRIPTORS edges=7 round1=7 round2=7 evaluations=15 stop_reason=EvaluationBudget workers=12
```

It passed in 163.56 s libtest time, with a 1m17s release rebuild and 241.493 s
wrapper elapsed. Transcript: `$env:TEMP\reda-task4-pregate.log`, SHA-256
`2ce0d0e216705accc7116f1ef682f165621fe5372ae87305c048929f14508d61`.
This gate reads the incumbent placements and proves only that descriptors are
offered; it cannot predict a routed or certified win. It was removed after
serving that purpose, and its symbol has zero source matches.

Because the count was nonzero, the required certified attribution test then
ran exactly once, with no alternate budget or rerun:

```text
PULL_X2_ATTRIBUTION range=14..21 descriptors=7 accepted=7 incumbent=QualityKey { observed_settle: 576, non_air_blocks: 71133, occupied_volume: 1401988, static_routed_delay: ExactDelay(678) } winner=QualityKey { observed_settle: 576, non_air_blocks: 71119, occupied_volume: 1401988, static_routed_delay: ExactDelay(678) } evaluations=21 candidate=da554e7386d5b6e0ce2babb36c5e37431f3864eca59447d9c1f4f94772cf588c workers=12
```

It passed in 229.17 s libtest time, with a 1m17s release rebuild and 307.210 s
wrapper elapsed. Transcript: `$env:TEMP\reda-task4-attribution.log`, SHA-256
`0389d61b8efa935fccbea5d1a1d9adf2130b856413b2f216da26220423877c7b`.
Every accepted entry was compared with the incumbent immediately before it and
was strictly lexicographically better. The retained ignored attribution test
repeats this assertion; Task 6 must not rerun it.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 32 | RED: round-2 focused filter, expected compile failure | 3.858 s | 697.244 s |
| 33 | GREEN: hierarchy API | not captured, floor 69.440 s | 766.684 s |
| 34 | GREEN: `block_pull_x` | 4.510 s | 771.194 s |
| 35 | GREEN: `refresh_relocation` | 3.696 s | 774.890 s |
| 36 | GREEN: search | 0.605 s | 775.495 s |
| 37 | one plan-only descriptor pre-gate | 241.493 s | 1016.988 s |
| 38 | one certified round-2 attribution run | 307.210 s | 1324.198 s |
| 39 | final hierarchy API after pre-gate removal | 80.717 s | 1404.915 s |

Task 4 adds **711.529 s** by the conservative floor accounting above. Used
after Task 4: **1404.915 s = 23.4 min** of the 60-minute wave budget. No
command reached the 600 s cap. The cap and wrapper elapsed observations are
controller metadata, not embedded in the transcript files.

### Candidates

| Candidate | Status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | **attempted, NO-GO** (1.18x vs 1.5x required) |
| 2: `prunable_parent_routes` sidecar | **attempted, GO, RETAINED** (26 ms to 13 ms median) |
| 3: hoisted merge consumer index | **attempted, NO-GO before production** (approved real probe had 0 merge gates and only 5 ms total work) |
| 4: filtered second bounded Pull-X round | **attempted, GO, RETAINED** (7/7 accepted round-2 proposals; 14 fewer blocks) |
| 5: one-shot bend-aware Refresh Relocation | PENDING |

## Task 4 test-boundary repair (post-hoc)

Retained Task 4 only; no Task 5 production work. Both refresh-stage `start`
test calculations in `hierarchy_api.rs` omitted `stream.pull_x2_edges`
length, unlike production's `seam`/`prune`/`refresh` boundary helpers, which
already summed it. The omission was latent because the driving fixtures
happened to freeze an empty round-2 vector.

`refresh_stage_alone_is_joint_quality_and_is_never_probed_early` was changed
to force a genuine, non-empty round-2 stage without inventing a new fixture:
it reuses `block_pull_x_proposal` (the same predicate the production freeze
closure uses) against the existing fixture's own edges to pick one that is
already round-2-legal, then sets the existing stream's `pull_x2_edges`
directly to `Some(vec![that edge])` before the walk runs.

RED, old `start`, transcript-backed (exit 101):

```text
assertion `left == right` failed: the stream ends one past the last refresh descriptor
  left: 12
 right: 11
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1041 filtered out; finished in 13.01s
```

Both `start` calculations were then fixed by adding the exact existing
`stream.pull_x2_edges.as_ref().map_or(0, Vec::len)` term. A second, interim
RED surfaced one level deeper: the same test's separate `bounded`
budget-cap stream is an independently constructed stream whose own
`pull_x2_edges` still froze empty (its own incumbent has no `dx != 0`
placements), so capping its budget at the now-larger `start` overran its true
Pass-4 boundary by one and reached Pass 5:

```text
a budget ending at the last Pass 4 proposal never probes Pass 5
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1041 filtered out; finished in 13.31s
```

Fixed by forcing that `bounded` stream's `pull_x2_edges` to the same forced
vector right after its own construction, reusing the same candidate edge --
no new fixture or helper. GREEN (exit 0):

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1041 filtered out; finished in 13.17s
```

The second `start` (in the `#[ignore]`d, release-only
`refresh_relocation_improves_an_acceptance_circuit`) received the identical
one-line fix by inspection and symmetry with production; per this repair's
"no release real-circuit run" constraint it was not executed in this
session.

### Focused and suite gates

Commands ran serially, each hard-capped at 600 s via POSIX `timeout 600`
(this session's shell tool is Git Bash, not PowerShell, so it is the
equivalent of `Invoke-Capped` rather than the plan's literal PowerShell
function). No command approached the cap. Transcript results (libtest's own
reported time and pass/fail counts) are distinguished from controller-observed
wrapper elapsed (Bash `SECONDS`, or "not captured" where the wrapper timing
itself failed to record).

| Command | Result | Transcript libtest time | Wrapper elapsed |
| --- | --- | --- | --- |
| `cargo test --lib block_pull_x` | 2 passed; 0 failed; 1 ignored | 3.73 s | 4 s |
| `cargo test --lib refresh_relocation` | 9 passed; 0 failed; 2 ignored | 3.00 s | 3 s |
| `cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture` | 30 passed; 1 failed; 7 ignored | 62.97 s | 63 s |
| `cargo test --lib compile::fragment_synth::hierarchy_api -- --nocapture --skip lowering_an_already_lowered_netlist_is_the_identity` | 30 passed; 0 failed; 7 ignored | 64.53 s | 65 s |

The one `hierarchy_api` failure, `lowering_an_already_lowered_netlist_is_the_identity`,
is a pre-existing environment defect unrelated to this repair -- it panics on
a Yosys/`yowasp-yosys` call with `RuntimeError: unsupported architecture for
wasmtime` from this machine's `wasmtime` install, and fails identically in
isolation with or without this repair's changes. The `--skip` run confirms
every other hierarchy_api test, including both tests named in this repair, is
green: 30 passed, 0 failed, 7 ignored, matching the retained Task 4 baseline
of 31 minus that one machine-local defect.

`rustfmt --edition 2021 src/compile/fragment_synth/hierarchy_api.rs` made no
changes (already formatted); `git diff --check` is clean; the diff touches
only `src/compile/fragment_synth/hierarchy_api.rs`, inside the `tests`
module, across the two named tests -- 1 file changed, 28 insertions(+), 1
deletion(-). `route_opt.rs` is untouched, and Task 2's `Arc<RoutedParent>`
reuse path and its three guarded `PHASE` diagnostics are byte-identical.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 40 | RED: forced pull_x2, old `start` | 13.01 s (transcript floor) | 1417.925 s |
| 41 | interim RED: `start` fixed, `bounded` not yet | 13.31 s (transcript floor) | 1431.235 s |
| 42 | GREEN: both fixes applied | 13.17 s (transcript floor) | 1444.405 s |
| 43 | GREEN: `block_pull_x` | 4 s | 1448.405 s |
| 44 | GREEN: `refresh_relocation` | 3 s | 1451.405 s |
| 45 | hierarchy_api suite (hit pre-existing Yosys failure) | 63 s | 1514.405 s |
| 46 | diagnostic isolation of the pre-existing failure | 0.10 s (transcript floor) | 1514.505 s |
| 47 | hierarchy_api suite, `--skip` the known-broken test | 65 s | 1579.505 s |

This repair adds **174.59 s** by the conservative floor/wrapper accounting
above. Used after this repair: **1579.505 s = 26.3 min** of the 60-minute
wave budget. No command reached the 600 s cap. Full evidence is in
`.superpowers/sdd/2026-09-11-five-low-cost-optimization-passes/task-4-boundary-repair-report.md`.

Commit: `fix: include pull-x2 in refresh test boundaries`.

**Wave verdict: retained 2 of 5 so far.**

---

## Task 5: Candidate 5 -- one-shot bend-aware Refresh Relocation -- attempted, GO

Branch taken: **GO, RETAINED**. The existing refresh relocation pass now gets
one bounded fallback after all straight attempts for a downstream repeater
fail: it remembers only the first legal route-owned bend cell, tries the same
three-cell replacement once, and keeps it only when exactly one repeater is
removed and every branch still carries through. The public boolean API,
proposal ordering, acceptance policy, and fingerprints are unchanged.

Independent implementation review found that the attribution-only replay
discarded prior prune/refresh failures and directly indexed the route map. The
minimum correction asserts exact replay success and uses a checked lookup.
Fresh focused tests passed 15/15 route-opt, 12/12 union, 9/9 refresh
compatibility with 2 release-only tests ignored, and 4/4 bend-specific tests.
The bounded rereview returned **APPROVED** with no remaining Critical or
Important finding.

The precommitted real-circuit gate ran exactly once and passed:

```text
RELOCATION accepted=2 bend_attributed=1 bend_offered=1
incumbent=QualityKey { observed_settle: 560, non_air_blocks: 71119, occupied_volume: 1401988, static_routed_delay: ExactDelay(670) }
winner=QualityKey { observed_settle: 550, non_air_blocks: 71119, occupied_volume: 1401988, static_routed_delay: ExactDelay(670) }
test result: ok. 1 passed; 0 failed; finished in 369.20s
```

The command took 453.828 s including release compilation. The accepted bend
candidate improves observed settling by 10; block count, occupied volume, and
static routed delay do not regress. No second attribution run was made.

### Wave command-time ledger (continued)

| # | Command | Elapsed | Running total |
| --- | --- | --- | --- |
| 48 | Task 5 implementation RED/GREEN and support checks (conservative total) | 115.000 s | 1694.505 s |
| 49 | Post-review `route_opt` plus `git diff --check` | 17.374 s | 1711.879 s |
| 50 | Post-review union and refresh focused gates | 8.580 s | 1720.459 s |
| 51 | Sole release bend-attribution gate | 453.828 s | 2174.287 s |

Used after Task 5: **2174.287 s = 36.2 min** of the 60-minute wave budget.
No command reached the 600 s cap.

| Candidate | Final task status |
| --- | --- |
| 1: palette-indexed `BlockFlags` memo | **attempted, NO-GO** (1.18x vs 1.5x required) |
| 2: `prunable_parent_routes` sidecar | **attempted, GO, RETAINED** (26 ms to 13 ms median) |
| 3: hoisted merge consumer index | **attempted, NO-GO before production** (0 merge gates, 5 ms total work) |
| 4: filtered second bounded Pull-X round | **attempted, GO, RETAINED** (7/7 accepted; 14 fewer blocks) |
| 5: one-shot bend-aware Refresh Relocation | **attempted, GO, RETAINED** (one attributed acceptance; settle 560 to 550) |

**Wave verdict: retained 3 of 5.**
