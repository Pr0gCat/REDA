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
| 5: insert-then-prune fallback for Refresh Relocation | PENDING |

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
