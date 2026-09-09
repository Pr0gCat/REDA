# Portable Incremental Simulator — Layer 0 attribution and Layer 1A retention

Spec: `docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md` (Revision 2, approved 2026-09-09)
Plan: `docs/superpowers/plans/2026-09-09-portable-incremental-simulator.md`
Baseline commit: `225d3e28c41616d5f975b740df60d1352f4512a7` (`225d3e2`).

Sections 1-10 record the Task 1 / Layer 0 attribution. The temporary
instrumentation that produced it was removed in the same commit that adds this
file; no counter, gate or disabled branch survives in the simulator hot path.
Section 11 records Task 2 / Layer 1A, which is **KEEP** on its clean wall-time
gate. Section 12 records the Step 7 post-retention re-attribution and the second
removal of the temporary instrumentation. Section 13 records Task 3 / Layer 1B
in full, including its Step 6 wall-time gate. **Layer 1B is REVERTED**: it missed
its own 10% top-exhaustive gate on `multiplier4` and was removed completely, so
the retained stack at the end of this document is still exactly Layer 1A.

## 1. What was measured, and in which units

Two different units appear below and are never mixed:

- **worker-nanoseconds** — the sum of per-vector spans taken on each
  certification worker's own thread. Several workers run at once, so these sums
  exceed any wall-clock interval. Per spec sections 5.1 and 5.5 they rank CPU
  work only. They never prove a wall-time gain, never trigger retention, and are
  never divided into a phase wall time or used as an Amdahl bound.
- **wall nanoseconds/milliseconds** — only `SIM_WORK ... baseline_build_wall_ns`
  (one pristine baseline construction) and the existing `PHASE` / `CIRCUIT`
  lines.

**The wall times in this report are not clean retention evidence.** Every run
below had `REDA_SIM_WORK_COUNTS` set, so `PHASE exhaustive` and `CIRCUIT ... in
...` include instrumentation overhead. Retention decisions must use the clean
phase-only binary and the alternating `B1,C1,C2,B2,B3,C3` protocol in spec
section 5.1. The gated wall figures are reproduced here only to identify each
sweep and to show that instrumentation did not distort the workload beyond
recognition.

Aggregate vector worker-ns, the ranking denominator, is exactly
`clone + drive + settle + check` over all vectors of one sweep. `bits_of` and
`enforce_event_cap` were deliberately left unmeasured and are **not** in that
denominator. The four component-scan spans and the topology-rebuild span are
nested *inside* `settle`, so they are reported as shares of the same
denominator, not added to it.

## 2. Benchmark command and environment

Executed serially, one Cargo process at a time, from a single prebuilt release
executable in a dedicated target directory:

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

Executable:
`C:\Users\LTY\AppData\Local\Temp\reda-sim-attribution-225d3e2\release\deps\reda-181582025761747b.exe`.
Raw transcripts: `reda-multiplier4-attribution.txt`,
`reda-ripple_adder8-attribution.txt`, `reda-alu8-attribution.txt` in `%TEMP%`.
All three cases reported `test result: ok. 1 passed; 0 failed`.

**CPU-core samples and peak working set** are the already-measured values from
spec section 1, taken on the clean phase-only binary during the `multiplier4`
top exhaustive phase: 9.72–10.70 CPU cores in use on a 12-logical-core host, 13
process threads, 430.7–437.9 MiB working set. They were not re-sampled during
these gated runs, and no new sampling is claimed.

## 3. Top-sweep selection

Per spec section 5.1, the top module's sweep is the final `PHASE exhaustive`
line before that case's `CIRCUIT` line, because `compile_hierarchical_scoped`
finishes child blocks before the top's `compile_module_with_blocks` and the
benchmark uses budget zero. The adjacent `WORK exhaustive_vectors` line is the
cross-check.

| Circuit | Compiled blocks | Final `PHASE exhaustive` before `CIRCUIT` | Vectors | Exhaustive sweep at the top? |
|---|---:|---|---:|---|
| `multiplier4` | 3 | `PHASE exhaustive 142909` (`states 256 world 3771x6x267`) | 256 | **Yes** |
| `ripple_adder8` | 2 | `PHASE exhaustive 138` (`states 0 world 2195x6x187`) | 0 | No |
| `alu8` | 3 | `PHASE exhaustive 485` (`states 0 world 4072x6x253`) | 0 | No |

This is a load-bearing finding. Only `multiplier4` certifies its **top** module
exhaustively; its top module has 8 inputs, so all 256 canonical masks run there.
`ripple_adder8` and `alu8` exceed the exhaustive input threshold at the top, so
their top modules run `WORK exhaustive_vectors 0` and their end-to-end time is
dominated by the manifest sweep (`PHASE manifest 7578` and `23836`
respectively, gated). Their exhaustive evidence below therefore comes from
**intermediate/leaf blocks**, and any exhaustive-path optimization is expected
to move `multiplier4` far more than the two regression controls.

## 4. Raw keyed lines

### 4.1 `multiplier4` — top sweep (the retention target)

```
SIM_WORK baseline states 256 world 3771x6x267 baseline_build_wall_ns 281931200
WORK exhaustive_workers 11
SIM_WORK sweep states 256 world 3771x6x267 vectors 256
SIM_WORK sweep states 256 world 3771x6x267 clone_worker_ns sum 2062160100 max 19003400 count 256
SIM_WORK sweep states 256 world 3771x6x267 drive_worker_ns sum 2191200 max 87700 count 256
SIM_WORK sweep states 256 world 3771x6x267 settle_worker_ns sum 1503420849000 max 7639562700 count 256
SIM_WORK sweep states 256 world 3771x6x267 check_worker_ns sum 105564500 max 3691000 count 256
SIM_WORK sweep states 256 world 3771x6x267 vector_total_worker_ns 1505590764800
SIM_WORK sweep states 256 world 3771x6x267 counts settle_iterations 244809 game_ticks 244553 due_events 18755499
SIM_WORK sweep states 256 world 3771x6x267 counts dirty_origins 189745183 active_dust 920978899 changed_dust 156674210
SIM_WORK sweep states 256 world 3771x6x267 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 256 world 3771x6x267 counts topology_cells 920978899 topology_probes 842804206
SIM_WORK sweep states 256 world 3771x6x267 counts torch_predicates 82500633 repeater_predicates 1475463843
SIM_WORK sweep states 256 world 3771x6x267 counts comparator_predicates 0 lamp_predicates 1958472
SIM_WORK sweep states 256 world 3771x6x267 scan_worker_ns torch 23806964300 repeater 199594338800 comparator 59639600 lamp 814385400
PHASE exhaustive 142909
WORK exhaustive_vectors 256
```

Gated wall context for the same case (**not** retention evidence):
`PHASE certify 156854`, `PHASE manifest 11506`, `PHASE layout+routing 11721`,
`CIRCUIT multiplier4 (hierarchical): OK gates=337 blocks_compiled=3 ticks=1039
blocks=124948 in 182.7469524s`. Quality is unchanged from spec section 1
(1,039 ticks / 124,948 blocks).

### 4.2 `multiplier4` — intermediate sweep

```
SIM_WORK baseline states 256 world 1171x6x123 baseline_build_wall_ns 68268300
WORK exhaustive_workers 12
SIM_WORK sweep states 256 world 1171x6x123 vectors 256
SIM_WORK sweep states 256 world 1171x6x123 clone_worker_ns sum 390867200 max 11060600 count 256
SIM_WORK sweep states 256 world 1171x6x123 drive_worker_ns sum 1622900 max 49500 count 256
SIM_WORK sweep states 256 world 1171x6x123 settle_worker_ns sum 120727687500 max 867983200 count 256
SIM_WORK sweep states 256 world 1171x6x123 check_worker_ns sum 37103700 max 4134200 count 256
SIM_WORK sweep states 256 world 1171x6x123 vector_total_worker_ns 121157281300
SIM_WORK sweep states 256 world 1171x6x123 counts settle_iterations 81826 game_ticks 81570 due_events 1528856
SIM_WORK sweep states 256 world 1171x6x123 counts dirty_origins 16463248 active_dust 75789568 changed_dust 12308920
SIM_WORK sweep states 256 world 1171x6x123 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 256 world 1171x6x123 counts topology_cells 75789568 topology_probes 71380104
SIM_WORK sweep states 256 world 1171x6x123 counts torch_predicates 8428078 repeater_predicates 95245464
SIM_WORK sweep states 256 world 1171x6x123 counts comparator_predicates 0 lamp_predicates 409130
SIM_WORK sweep states 256 world 1171x6x123 scan_worker_ns torch 1847718300 repeater 13370833300 comparator 11828900 lamp 125228000
PHASE exhaustive 11062
WORK exhaustive_vectors 256
```

### 4.3 `multiplier4` — leaf sweep

```
SIM_WORK baseline states 8 world 255x6x79 baseline_build_wall_ns 5096600
WORK exhaustive_workers 1
SIM_WORK sweep states 8 world 255x6x79 vectors 8
SIM_WORK sweep states 8 world 255x6x79 clone_worker_ns sum 1529300 max 210800 count 8
SIM_WORK sweep states 8 world 255x6x79 drive_worker_ns sum 15500 max 3000 count 8
SIM_WORK sweep states 8 world 255x6x79 settle_worker_ns sum 103970400 max 13903900 count 8
SIM_WORK sweep states 8 world 255x6x79 check_worker_ns sum 182700 max 44500 count 8
SIM_WORK sweep states 8 world 255x6x79 vector_total_worker_ns 105697900
SIM_WORK sweep states 8 world 255x6x79 counts settle_iterations 650 game_ticks 642 due_events 4296
SIM_WORK sweep states 8 world 255x6x79 counts dirty_origins 41814 active_dust 138439 changed_dust 28828
SIM_WORK sweep states 8 world 255x6x79 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 8 world 255x6x79 counts topology_cells 138439 topology_probes 118122
SIM_WORK sweep states 8 world 255x6x79 counts torch_predicates 16250 repeater_predicates 88400
SIM_WORK sweep states 8 world 255x6x79 counts comparator_predicates 0 lamp_predicates 1300
SIM_WORK sweep states 8 world 255x6x79 scan_worker_ns torch 1457600 repeater 5036500 comparator 28500 lamp 201700
PHASE exhaustive 111
WORK exhaustive_vectors 8
```

### 4.4 `ripple_adder8`

Leaf sweep (the only exhaustive sweep in this case):

```
SIM_WORK baseline states 8 world 255x6x79 baseline_build_wall_ns 8104500
WORK exhaustive_workers 1
SIM_WORK sweep states 8 world 255x6x79 vectors 8
SIM_WORK sweep states 8 world 255x6x79 clone_worker_ns sum 1408500 max 197700 count 8
SIM_WORK sweep states 8 world 255x6x79 drive_worker_ns sum 12000 max 2100 count 8
SIM_WORK sweep states 8 world 255x6x79 settle_worker_ns sum 98936500 max 13652900 count 8
SIM_WORK sweep states 8 world 255x6x79 check_worker_ns sum 139700 max 31300 count 8
SIM_WORK sweep states 8 world 255x6x79 vector_total_worker_ns 100496700
SIM_WORK sweep states 8 world 255x6x79 counts settle_iterations 650 game_ticks 642 due_events 4296
SIM_WORK sweep states 8 world 255x6x79 counts dirty_origins 41814 active_dust 138439 changed_dust 28828
SIM_WORK sweep states 8 world 255x6x79 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 8 world 255x6x79 counts topology_cells 138439 topology_probes 118122
SIM_WORK sweep states 8 world 255x6x79 counts torch_predicates 16250 repeater_predicates 88400
SIM_WORK sweep states 8 world 255x6x79 counts comparator_predicates 0 lamp_predicates 1300
SIM_WORK sweep states 8 world 255x6x79 scan_worker_ns torch 1373400 repeater 4804300 comparator 25700 lamp 186800
PHASE exhaustive 109
WORK exhaustive_vectors 8
```

Top block, no exhaustive sweep:

```
SIM_WORK baseline states 0 world 2195x6x187 baseline_build_wall_ns 138507200
PHASE exhaustive 138
WORK exhaustive_vectors 0
PHASE manifest 7578
```

`CIRCUIT ripple_adder8 (hierarchical): OK gates=200 blocks_compiled=2 ticks=608
blocks=70603 in 13.8777139s` (gated). Quality matches spec section 1
(608 ticks / 70,603 blocks).

### 4.5 `alu8`

First block, the only exhaustive sweep in this case:

```
SIM_WORK baseline states 128 world 426x6x139 baseline_build_wall_ns 18543400
WORK exhaustive_workers 12
SIM_WORK sweep states 128 world 426x6x139 vectors 128
SIM_WORK sweep states 128 world 426x6x139 clone_worker_ns sum 74766800 max 3858400 count 128
SIM_WORK sweep states 128 world 426x6x139 drive_worker_ns sum 650500 max 14700 count 128
SIM_WORK sweep states 128 world 426x6x139 settle_worker_ns sum 11011705100 max 104177000 count 128
SIM_WORK sweep states 128 world 426x6x139 check_worker_ns sum 6845100 max 323200 count 128
SIM_WORK sweep states 128 world 426x6x139 vector_total_worker_ns 11093967500
SIM_WORK sweep states 128 world 426x6x139 counts settle_iterations 16766 game_ticks 16638 due_events 169492
SIM_WORK sweep states 128 world 426x6x139 counts dirty_origins 1997726 active_dust 7239056 changed_dust 1305582
SIM_WORK sweep states 128 world 426x6x139 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 128 world 426x6x139 counts topology_cells 7239056 topology_probes 6272072
SIM_WORK sweep states 128 world 426x6x139 counts torch_predicates 804768 repeater_predicates 7947084
SIM_WORK sweep states 128 world 426x6x139 counts comparator_predicates 0 lamp_predicates 33532
SIM_WORK sweep states 128 world 426x6x139 scan_worker_ns torch 142088400 repeater 964871400 comparator 1626100 lamp 13974000
PHASE exhaustive 1037
WORK exhaustive_vectors 128
```

Second and top blocks, no exhaustive sweep:

```
SIM_WORK baseline states 0 world 1969x6x187 baseline_build_wall_ns 150939700
PHASE exhaustive 150
WORK exhaustive_vectors 0
PHASE manifest 3944
SIM_WORK baseline states 0 world 4072x6x253 baseline_build_wall_ns 485331300
PHASE exhaustive 485
WORK exhaustive_vectors 0
PHASE manifest 23836
```

`CIRCUIT alu8 (hierarchical): OK gates=400 blocks_compiled=3 ticks=972
blocks=213833 in 48.8649879s` (gated). Quality matches spec section 1
(972 ticks / 213,833 blocks).

## 5. Same-unit worker-ns shares

Denominator per sweep is that sweep's `vector_total_worker_ns`. For all four
sweeps the four span sums add to the printed total exactly, so no vector was
lost or double-counted. The four scan rows and the topology row are nested
inside `settle` and are shown as shares of the same denominator.

### 5.1 `multiplier4` top sweep — 1,505,590,764,800 worker-ns over 256 vectors

| Span | Sum (ns) | Max (ns) | Share |
|---|---:|---:|---:|
| clone | 2,062,160,100 | 19,003,400 | 0.1370% |
| drive | 2,191,200 | 87,700 | 0.0001% |
| settle | 1,503,420,849,000 | 7,639,562,700 | 99.8559% |
| check | 105,564,500 | 3,691,000 | 0.0070% |

Inside `settle`:

| Nested span | Sum (ns) | Share of total |
|---|---:|---:|
| repeater mismatch scan | 199,594,338,800 | 13.2569% |
| torch mismatch scan | 23,806,964,300 | 1.5812% |
| lamp mismatch scan | 814,385,400 | 0.0541% |
| comparator mismatch scan | 59,639,600 | 0.0040% |
| **all four component scans** | **224,275,328,100** | **14.8962%** |
| dust topology rebuild | 0 | 0.0000% |
| **settle remainder** (dust recompute + due-event application + queue) | **1,279,145,520,900** | **84.9597%** |

### 5.2 Other sweeps

| Sweep | Total worker-ns | clone | drive | settle | check | four scans | settle remainder |
|---|---:|---:|---:|---:|---:|---:|---:|
| `multiplier4` intermediate (256 vec) | 121,157,281,300 | 0.3226% | 0.0013% | 99.6454% | 0.0306% | 12.6741% | 86.9713% |
| `alu8` block (128 vec) | 11,093,967,500 | 0.6739% | 0.0059% | 99.2585% | 0.0617% | 10.1187% | 89.1398% |
| `ripple_adder8` leaf (8 vec) | 100,496,700 | 1.4015% | 0.0119% | 98.4475% | 0.1390% | 6.3586% | 92.0889% |
| `multiplier4` leaf (8 vec) | 105,697,900 | 1.4468% | 0.0147% | 98.3656% | 0.1729% | 6.3618% | 92.0038% |

The two leaf sweeps are the same child module compiled in two different cases.
Their simulator work counts are byte-identical across two separate processes —
`settle_iterations 650`, `game_ticks 642`, `due_events 4296`,
`dirty_origins 41814`, `active_dust 138439`, `changed_dust 28828`,
`topology_cells 138439`, `topology_probes 118122`, `torch_predicates 16250`,
`repeater_predicates 88400`, `comparator_predicates 0`, `lamp_predicates 1300`
— which is an independent determinism cross-check on the counter placement.

## 6. Measured ranking and spec section 5.5 outcomes

Ranking for the `multiplier4` top sweep, by same-unit worker-ns share:

1. **Settle remainder — 84.96%.** Dust recompute plus due-event application.
   Not further subdivided by this instrumentation.
2. **Repeater mismatch scan — 13.26%.**
3. **Torch mismatch scan — 1.58%.**
4. **Clone — 0.14%.**
5. **Lamp mismatch scan — 0.05%.**
6. **Check — 0.007%.**
7. **Comparator mismatch scan — 0.004%.**
8. **Drive — 0.0001%.**
9. **Dust topology rebuild — 0.000%.**

Applying spec section 5.5 to those shares:

| Section 5.5 branch | Threshold | Measured | Outcome |
|---|---|---:|---|
| Full dust-topology rebuild → separate local-rebuild design before Layer 2 | ≥10% | **0.000%** | **Not eligible.** No local-rebuild design is written. |
| Component scans → Layer 2 eligible | ≥10% | **14.90%** | **Eligible.** Layer 2 may be reached if earlier layers do not close the target. |
| Cloning → separate per-worker reset design | ≥10% | **0.137%** | **Not eligible.** No snapshot/rollback or undo-journal design; spec section 3's conditional non-goal stays in force. |

Two spec section 1 hypotheses are corrected by measurement:

- **Hypothesis 2 is refuted.** Spec section 1 expected a full dust-topology
  rebuild for every exhaustive mask containing a high pinned input. Measured
  `topology_rebuilds` is **0** in every sweep of all three circuits: driving the
  canonical inputs never invalidates the shared topology cache. The
  already-retained cache from `f6e54ea` is doing its job, and incremental
  topology rebuild is not a candidate.
- **Hypothesis 1 is heavily de-prioritised.** Cloning the pristine `Simulator` is
  0.137% of the top sweep's vector worker-ns — three orders of magnitude below
  the 10% bar. Clone cost falls with vector size, not rises: it is 1.45% of the
  8-vector leaf sweep and 0.14% of the 256-vector top sweep.

Supporting counters for the two Layer 1 candidates, `multiplier4` top sweep:

- **Layer 1A (discard the cached recompute's derived dust dirty set).**
  `changed_dust` 156,674,210 of `dirty_origins` 189,745,183 — derived dust
  write-back accounts for **82.57%** of every dirty origin consumed. Each dirty
  origin is expanded two hops and floods its whole component, which is what
  produces `active_dust` 920,978,899 (3,597,574 dust cells revisited per vector)
  and `topology_probes` 842,804,206. This is the mechanism behind the 84.96%
  settle remainder, and it is the strongest measured target.
- **Layer 1B (skip mismatch scans on empty due-event ticks).** The scans it
  removes are the 14.90% measured above, and `repeater_predicates`
  1,475,463,843 (5,763,531 per vector) shows why the repeater scan dominates
  them. However `due_events` 18,755,499 over `game_ticks` 244,553 averages
  **76.69 due events per game tick**, so ticks are not empty on average. The
  aggregate cannot resolve what fraction of individual ticks are empty, so
  Layer 1B's premise is neither confirmed nor refuted here. It must be measured
  by its own wall-time gate before retention.

## 7. Caveats

Carried from the Opus pre-benchmark review and from the instrumentation's own
limits. All were known before the benchmark ran.

1. **Gated wall time is not retention evidence.** Every `PHASE` and `CIRCUIT`
   value in this report was produced with `REDA_SIM_WORK_COUNTS` set. The
   `SIM_WORK baseline ...` line is emitted between `fresh_simulator` and
   `phase("exhaustive", ...)`, so it is inside the measured `PHASE exhaustive`
   window. Retention uses the clean phase-only binary only.
2. **Worker-ns ranks; it never retains.** These sums exceed wall time by roughly
   the worker count (`WORK exhaustive_workers 11` for the top sweep). They may
   not be divided into a phase wall time or used as an Amdahl or physical-limit
   bound, per spec sections 5.1 and 5.5.
3. **`topology_cells` duplicates `active_dust`.** The plan pinned
   `topology_cells` to `active_dust.len()`, and the simulator only ever uses
   `recompute_dust_strengths_cached`, so the two fields are one measurement
   reported twice, not two independent ones. This was raised before the run and
   accepted; it changes no conclusion because both feed the same finding.
4. **The settle remainder is not subdivided.** 84.96% of the top sweep is
   attributed to "dust recompute + due-event application + queue" as one block.
   The counters (`dirty_origins`, `active_dust`, `changed_dust`,
   `topology_probes`, `due_events`) point inside it, but no span separates dust
   recompute from event application. If Layer 1A misses its gate, the next
   attribution round must split that span before another candidate is chosen.
5. **A dirty-origin residual is unexplained.** `dirty_origins` minus
   `changed_dust` is 33,070,973, which exceeds `due_events` 18,755,499 — the
   upper bound on component writes — by 14,315,474. `World::dirty` is a set, so
   duplicates within one `take_dirty` round cannot explain it. Layer 1A's
   correctness premise ("any dirty entries left immediately after write-back are
   exactly the derived wire-power writes", spec section 5.2) therefore must be
   proved by its own TDD oracle rather than assumed from these aggregates.
6. **Instrumented single samples.** Each circuit ran once. `multiplier4` at
   182.7469524 s gated sits near its clean three-sample median of 179.9921003 s
   and its top exhaustive 142.909 s gated sits inside the clean 139.193–145.296 s
   range, which shows the instrumentation did not distort the workload — but one
   gated sample is not a baseline and is not used as one.
7. **`ripple_adder8` and `alu8` do not exercise the top-module exhaustive path.**
   Their top modules report `WORK exhaustive_vectors 0`. They remain the
   regression controls at their fixed ceilings of 15.36352587 s and
   54.17791974 s, but they cannot confirm an exhaustive-path speedup.

## 8. Reviewer verdicts

- **Plan review (recorded in the SDD ledger):** round 1 NOT APPROVED with
  blockers B1–B5, F1–F6 and P3; re-review left one blocker on `CachedDustResult`
  visibility and the per-direction probe count; final scoped re-review APPROVED
  by Claude Opus with no remaining blockers.
- **Task 1 pre-benchmark instrumentation review (Opus), round 1: NOT APPROVED.**
  Two blockers and two cleanups:
  1. *(blocker)* the sweep summary used separate `eprintln!` calls and could
     interleave across concurrent hierarchical leaf compiles;
  2. *(blocker)* `SIM_WORK baseline_build_wall_ns` was unkeyed and not pairable
     with its sweep;
  3. *(cleanup)* baseline work counts were snapshotted after the clone timer had
     started;
  4. *(cleanup)* a stale comment claimed the sweep reduces `()`.
- **Fixes applied:** the whole block is joined into one `String` and written by a
  single `eprintln!` (one stderr lock acquisition) with the
  `sweep states {n} world {X}x{Y}x{Z}` key repeated on every line; the baseline
  line carries the same key; the counts snapshot moved ahead of the clone timer;
  the comment corrected. The two focused Step 4 commands were rerun after the
  fixes — 1 passed and 27 passed, both exit 0 — and the attribution above was
  then benchmarked from the reviewed revision.
- **Post-fix verdict: approved to benchmark.** The three raw transcripts in this
  report are the output of that approved revision.

## 9. Instrumentation removal

All Layer 0 instrumentation was removed in the same commit that adds this
report. `src/compile/fragment_synth/certification.rs`,
`src/redstone/simulator/mod.rs` and `src/redstone/simulator/propagate.rs` were
restored byte-for-byte to `225d3e2`, since instrumentation was the only change
those files carried.

```
rg -n "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
```

finds nothing, so no counter field, gate read or disabled branch remains in the
simulator hot path. Production candidates are benchmarked from a clean
phase-only binary, exactly as spec section 5.1 requires.

## 10. Next step

Layer 1A is the next candidate on the measured evidence: it targets the 82.57%
of dirty origins that are the cached recompute's own derived dust write-back,
inside the 84.96% settle remainder. Layer 1B follows, targeting the 14.90%
component scans, with its empty-tick premise still to be established. Layer 2 is
eligible on the component-scan share but is reached only if the retained Layer 1
stack does not close the 1.5x end-to-end target. No per-worker reset or
incremental topology-rebuild design is written: both failed their 10% eligibility
bar by three orders of magnitude.

Each candidate is implemented independently, gated on `multiplier4`
top-exhaustive **wall** time from a clean binary, and removed if it misses.

## 11. Layer 1A (Task 2): implementation, semantic tests, the wide differential suite and the clean wall-time gate — **KEEP**

### 11.1 Decision

**KEEP.** Layer 1A passes its retention gate by a wide margin: `multiplier4`
top-exhaustive median wall time falls from **129,185 ms** to **49,258 ms**, a
**2.6226x** speedup and a **61.8702%** reduction, against a required reduction of
at least 10% (`median(C) <= 0.90 * median(B)` = 116,266.5 ms). Both regression
controls are far below their fixed ceilings and both got faster, so there is no
regression to weigh. Every case reproduced identical gates, compiled blocks,
ticks and blocks.

The mandatory ignored release full-resettle suite (spec section 5.2) **passes**:
5 passed, 0 failed, exit 0. Every semantic precondition the spec sets for
retention is therefore met, not just the narrow ones.

Step 7's post-retention re-attribution is complete in section 12. The retained
revision still spends 36.2590% of top-sweep vector worker time in the four
component scans, so the plan's measured precondition for attempting Task 3 is
met. Task 3 has not been started here and still owes its own wall-time gate.

### 11.2 The change

One call site in `recompute_dust_strengths_cached`
(`src/redstone/simulator/propagate.rs`) calls the existing `World::take_dirty()`
once after `recompute_active_dust` returns, discarding the derived dust
write-back entries. Public `recompute_dust_strengths`, `World::set`,
palette/index maintenance and topology epochs are unchanged; no writer
abstraction was added.

The premise holds structurally at this call site: the function itself already
consumed all pre-existing dirt with a destructive `take_dirty`, and it holds the
only `&mut World` borrow throughout, so the sole writer in between is the
write-back loop. The discarded entries are therefore exactly the derived
wire-power writes, and every one of them is simultaneously returned in `changed`
to `settle_from_current_state`, which is what actually schedules mismatched
components. The dirty set was carrying a redundant second copy whose only effect
was to make the next cached recompute re-select and re-solve a component that had
just reached its fixed point.

### 11.3 TDD, semantic tests and the mandatory wide differential suite

`cached_recompute_returns_changes_without_redirtying_its_own_writeback` failed
only on its final dirty-set assertion before the change (exit 101,
`cached write-back must consume only its own dirt`, at `propagate.rs:1027`) and
passes after it. Critically, the two equivalence assertions that run first —
sorted changed positions equal to the public path's, and every world cell equal
to the public path's — passed in **both** the RED and GREEN states, so the discard
changed neither the returned changed set nor the final world.

| Command | Result |
|---|---|
| `cargo test --lib redstone::simulator::propagate::tests -- --nocapture` | 20 passed, 0 failed |
| `cargo test --lib redstone::simulator::differential::tests -- --nocapture` | 4 passed, 0 failed |
| `cargo test --lib compile::resettle_differential::the_reported_stale_dust_case_settles_clean -- --exact` | 1 passed, 0 failed |
| `cargo test --lib compile::resettle_differential::and4s_full_sweep_is_differential_clean -- --exact` | 1 passed, 0 failed |

The mandatory wide suite ran before the wall gate:

```
cargo test --release --lib compile::resettle_differential -- --ignored --nocapture --test-threads=1

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 1009 filtered out; finished in 55.31s
EXIT=0
```

Spec section 5.2 names these oracles mandatory rather than optional measurement
fixtures, because they deliberately remove the old self-dirty safety net — they
are the harnesses that would expose an incomplete active-set selection. All five
report zero stale cells:

| Oracle | Evidence |
|---|---|
| 240 `and4` transitions | **0 / 480 stale** |
| injected isolation | **0 / 4758 stale**, and **0 stale ZERO** |
| baseline isolation | `and4` **0 / 11**, `full_adder` **0 / 25**, negotiated `full_adder` **0 / 25** |
| negotiated plan | `full_adder` **0 / 8 vectors stale**; `segment_a` did not route and was **NOT MEASURED** |
| six-condition | `and4`, `full_adder`, `segment_a`, `seven_segment`, `verilog:and4`, `verilog:seven_segment` — all **0 stale cells** and **0 output moves** |

The 4758-cell injected-isolation figure is the sharpest of these: the injection
deliberately manufactures stale dust, and the candidate still resolved every
injected cell, including the separately counted stale-`ZERO` class. The
six-condition harness additionally checks **0 output moves**, so no pinned output
changed position.

One coverage limit is recorded rather than smoothed over: in the negotiated-plan
oracle `segment_a` **did not route**, so that circuit contributed no measurement
on that harness. It is covered by the six-condition harness, which reports
`segment_a` clean.

### 11.4 Benchmark protocol as executed

Clean phase-only binaries, no `REDA_SIM_WORK_COUNTS`, no build between samples.
Baseline `B` executed in a detached worktree at `225d3e2` with its own
`CARGO_TARGET_DIR`; candidate `C` executed from the active worktree with a
separate `CARGO_TARGET_DIR`. Eighteen samples, six per case, in the mandated
`B1,C1,C2,B2,B3,C3` order, one Cargo process at a time.

The literal completion order of all eighteen samples confirms both the per-case
sequence and strict serialization — no two samples overlap:

```
17:44:22 multiplier4 B1     17:53:44 ripple_adder8 B1     17:55:29 alu8 B1
17:45:37 multiplier4 C1     17:53:55 ripple_adder8 C1     17:56:08 alu8 C1
17:46:52 multiplier4 C2     17:54:05 ripple_adder8 C2     17:56:46 alu8 C2
17:49:37 multiplier4 B2     17:54:18 ripple_adder8 B2     17:57:33 alu8 B2
17:52:21 multiplier4 B3     17:54:32 ripple_adder8 B3     17:58:20 alu8 B3
17:53:31 multiplier4 C3     17:54:42 ripple_adder8 C3     17:58:58 alu8 C3
```

Raw logs: `%TEMP%\reda-layer1a-{multiplier4|ripple_adder8|alu8}-{B1|C1|C2|B2|B3|C3}.txt`.

**Provenance cross-check.** Every `B` sample reports `1012 filtered out` and
every `C` sample reports `1013 filtered out`. The candidate carries exactly one
extra test — the Layer 1A oracle — so the harness counts independently confirm
that all nine `B` samples really executed the baseline checkout and all nine `C`
samples the candidate. All eighteen report `test result: ok. 1 passed; 0 failed`.

### 11.5 Raw values — all eighteen samples

Top exhaustive is the final `PHASE exhaustive` before that case's `CIRCUIT` line,
with the immediately following `WORK exhaustive_vectors` line quoted as the
extraction guard (spec section 5.1 / section 3).

`multiplier4` — all three modules carry exhaustive vectors:

| Sample | leaf `PHASE exhaustive` (8 vec) | intermediate (256 vec) | **top `PHASE exhaustive`** | guard | `CIRCUIT` |
|---|---:|---:|---:|---|---|
| B1 | 106 | 9891 | **133783** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 171.3011467s` |
| C1 | 43 | 3460 | **50059** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 74.657229s` |
| C2 | 38 | 3602 | **49258** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 73.8856725s` |
| B2 | 107 | 9675 | **129185** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 165.6771986s` |
| B3 | 105 | 9557 | **127047** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 163.0727452s` |
| C3 | 38 | 3541 | **46515** | `WORK exhaustive_vectors 256` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 70.3501778s` |

`ripple_adder8` — the top module certifies with **zero** exhaustive vectors:

| Sample | child (8 vec) | **top `PHASE exhaustive`** | guard | `CIRCUIT` |
|---|---:|---:|---|---|
| B1 | 105 | **196** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 13.2173563s` |
| C1 | 37 | **215** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 10.0056295s` |
| C2 | 37 | **194** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 9.8993233s` |
| B2 | 113 | **134** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 13.3594442s` |
| B3 | 106 | **213** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 13.2968716s` |
| C3 | 37 | **193** | `WORK exhaustive_vectors 0` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603 in 9.8902353s` |

`alu8` — the first module carries 128 vectors; the second and top carry zero:

| Sample | module 1 (128 vec) | module 2 (0 vec) | **top `PHASE exhaustive`** | guard | `CIRCUIT` |
|---|---:|---:|---:|---|---|
| B1 | 1020 | 147 | **454** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 47.5957969s` |
| C1 | 333 | 144 | **452** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 38.4901755s` |
| C2 | 338 | 160 | **441** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 37.758613s` |
| B2 | 1004 | 146 | **450** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 47.1626962s` |
| B3 | 1016 | 159 | **449** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 46.9833758s` |
| C3 | 344 | 144 | **441** | `WORK exhaustive_vectors 0` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833 in 37.747627s` |

### 11.6 Medians and the retention gate

Medians are over the three samples of each arm, per spec section 5.1: retention
uses the ratio of medians, not the median of paired ratios.

**The gate — `multiplier4` top exhaustive:**

| Arm | Samples (ms, sorted) | Median (ms) |
|---|---|---:|
| Baseline `B` | 127047, 129185, 133783 | **129185** |
| Candidate `C` | 46515, 49258, 50059 | **49258** |

- Speedup `median(B)/median(C)` = 129185 / 49258 = **2.622620x**
- Reduction = **61.8702%**
- Gate threshold `0.90 * median(B)` = 116,266.5 ms; candidate median 49,258 ms — **PASS**, with 6.19x the required reduction.

**End-to-end medians:**

| Case | `B` samples (s, sorted) | `B` median | `C` samples (s, sorted) | `C` median | Speedup | Ceiling | Under ceiling |
|---|---|---:|---|---:|---:|---:|---|
| `multiplier4` | 163.0727452, 165.6771986, 171.3011467 | **165.6771986** | 70.3501778, 73.8856725, 74.657229 | **73.8856725** | **2.242345x** | 119.9947335 (Goal 1) | yes |
| `ripple_adder8` | 13.2173563, 13.2968716, 13.3594442 | **13.2968716** | 9.8902353, 9.8993233, 10.0056295 | **9.8993233** | 1.343210x | 15.36352587 | yes |
| `alu8` | 46.9833758, 47.1626962, 47.5957969 | **47.1626962** | 37.747627, 37.758613, 38.4901755 | **37.758613** | 1.249058x | 54.17791974 | yes |

No representative regression exists in either direction to test against the 5%
bar: both controls are **faster** on the candidate, by 25.5515% (`ripple_adder8`)
and 19.9397% (`alu8`). Both arms of both controls sit under their fixed ceilings.

**Paired ratios** (`B1/C1`, `B2/C2`, `B3/C3`), reported only to expose drift and
never used for retention:

| Case | metric | B1/C1 | B2/C2 | B3/C3 |
|---|---|---:|---:|---:|
| `multiplier4` | top exhaustive | 2.672506 | 2.622620 | 2.731312 |
| `multiplier4` | end to end | 2.294502 | 2.242345 | 2.318015 |
| `ripple_adder8` | end to end | 1.320992 | 1.349531 | 1.344444 |
| `alu8` | end to end | 1.236570 | 1.249058 | 1.244671 |

The paired ratios are tight around the ratio of medians in every row, so the
result is not an artifact of drift between arms.

### 11.7 Identical-results evidence

Every sample of a case reproduced a byte-identical `CIRCUIT` quality key, across
both arms:

| Case | gates | blocks_compiled | ticks | blocks | identical across |
|---|---:|---:|---:|---:|---|
| `multiplier4` | 337 | 3 | 1039 | 124948 | all 6 samples |
| `ripple_adder8` | 200 | 2 | 608 | 70603 | all 6 samples |
| `alu8` | 400 | 3 | 972 | 213833 | all 6 samples |

Tick counts are the sharpest of these: 1039, 608 and 972 game ticks reproduce
exactly, so the candidate did not shorten, lengthen or reorder any settle. Every
sample also reproduced its module's `WORK exhaustive_vectors` and
`WORK manifest_transitions` counts, so no vector or transition was skipped.

### 11.8 Where the time went

`multiplier4`'s B2 and C2 samples are simultaneously the median for top
exhaustive and for end to end, so they decompose the win without mixing runs:

| Span | B2 | C2 | Delta | Share of the 91.7915261 s end-to-end delta |
|---|---:|---:|---:|---:|
| top exhaustive | 129185 ms | 49258 ms | 79.927 s | 87.07% |
| intermediate exhaustive (256 vec) | 9675 ms | 3602 ms | 6.073 s | 6.62% |
| top manifest | 11093 ms | 6118 ms | 4.975 s | 5.42% |
| leaf exhaustive (8 vec) | 107 ms | 38 ms | 0.069 s | 0.08% |

Routing, placement, structure/emit/verify, timing, equivalence and
metrics/fingerprints are unchanged within noise (top-module `route_nets`
10870.672 ms vs 10729.244 ms; `metrics+fingerprints` 52 ms in both), which is
expected: Layer 1A touches only the simulator.

Two corroborating observations, both consistent with a general simulator
improvement rather than a fixture-specific one:

- **Every module that actually runs exhaustive vectors improved by a similar
  factor**, independent of size: `multiplier4` leaf 2.7895x (8 vectors),
  `multiplier4` intermediate 2.7323x (256), `multiplier4` top 2.6226x (256),
  `ripple_adder8` child 2.8649x (8), `alu8` module 1 3.0059x (128).
- **The manifest phase also improved**, because it drives the same simulator:
  top-module manifest medians 11326 → 6118 ms (1.8513x) for `multiplier4`,
  7049 → 3810 ms (1.8501x) for `ripple_adder8` and 22900 → 16069 ms (1.4251x)
  for `alu8`.

The second point is what explains the control speedups. Task 1's open concern 4
warned that `ripple_adder8` and `alu8` certify their top modules with
`WORK exhaustive_vectors 0`; that remains true, and their **top exhaustive**
medians are accordingly noise on a sub-250 ms span (196 → 194 ms and 450 →
441 ms) and are **not** evidence of a speedup. Their end-to-end gains come from
the manifest sweep and from `alu8`'s 128-vector child module, not from their top
exhaustive phase. Their role in the gate is unchanged: they bound regression
against fixed ceilings, and they do so comfortably.

### 11.9 Goal 1 status and caveats

The candidate `multiplier4` end-to-end median of **73.8856725 s** is below Goal
1's absolute target of 119.9947335 s. Measured against the spec section 1 fixed
baseline of 179.9921003 s that is 2.436089x; measured against this session's own
baseline arm it is 2.242345x. Both readings clear the 1.5x objective, and the
same-session figure is the conservative one.

Caveats that travel with this result:

1. **This session's baseline arm is faster than the spec's fixed baseline**
   (165.6771986 s vs 179.9921003 s, an 8.0% difference in host conditions
   between sessions). The retention decision is unaffected — the gate is
   same-session `B` vs `C` — but the fixed ceilings and the 179.9921003 s
   figure come from an earlier session and should not be treated as
   interchangeable with today's `B` arm.
2. **One negotiated-plan circuit was not measured.** `segment_a` did not route
   in that oracle, so it contributed no stale-cell count there. The
   six-condition harness covers `segment_a` and reports it clean, but the
   negotiated-plan path itself is unexercised for that circuit.
3. **Step 7 supports attempting Task 3, not retaining it.** Section 12 records
   fresh clone/drive/settle/check, dust/topology and per-kind component shares;
   scans are 36.2590% of vector worker time. Task 3 therefore has measured
   justification to run, but must still pass its independent 10% wall-time and
   semantic gates before any code is kept.
4. **Task 1's open concern 1 is now partly settled by measurement.** The 84.96%
   unsubdivided settle remainder did contain the dominant removable work: 61.87%
   of top-exhaustive wall time was the redundant re-solve caused by the derived
   dirty entries. What remains inside that span is still unsubdivided.

## 12. Step 7 phase B: post-retention re-attribution on the retained Layer 1A

Spec section 5.5 requires fresh attribution on the retained revision before the
next layer is chosen. The Task 1 instrumentation was reapplied over the retained
`6ccb7a7` (temporary commit `8b7fe10`), `multiplier4` was run once with both
diagnostic environment variables, and all temporary code was then removed.

### 12.1 Run status and the wrapper caveat

The libtest run **passed**:

```
test compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1014 filtered out; finished in 72.01s
```

with the quality key unchanged:

```
CIRCUIT multiplier4 (hierarchical): OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948 in 72.0029272s
```

337 gates, 3 compiled blocks, 1,039 ticks and 124,948 blocks are the same values
recorded in section 11.7 for every Layer 1A benchmark sample and in spec section
1 for the original baseline.

**Wrapper caveat.** The controller's PowerShell wrapper returned exit code 1.
This is **not** a REDA failure: PowerShell promoted the native stderr warning
records emitted after the successful test into terminating error records. The
authoritative result is the libtest line above — 1 passed, 0 failed. No test
failed, no assertion fired, and the circuit certified.

Raw log: `%TEMP%\reda-multiplier4-layer1a-reattribution.txt`.

### 12.2 Raw keyed lines — top sweep

Selected as in section 3: the final keyed sweep before the `CIRCUIT` line, cross
checked by `WORK exhaustive_vectors 256`.

```
SIM_WORK baseline states 256 world 3771x6x267 baseline_build_wall_ns 274814900
SIM_WORK sweep states 256 world 3771x6x267 vectors 256
SIM_WORK sweep states 256 world 3771x6x267 clone_worker_ns sum 1732661900 max 14889500 count 256
SIM_WORK sweep states 256 world 3771x6x267 drive_worker_ns sum 2128300 max 20500 count 256
SIM_WORK sweep states 256 world 3771x6x267 settle_worker_ns sum 498619097100 max 2240959400 count 256
SIM_WORK sweep states 256 world 3771x6x267 check_worker_ns sum 94780200 max 3401500 count 256
SIM_WORK sweep states 256 world 3771x6x267 vector_total_worker_ns 500448667500
SIM_WORK sweep states 256 world 3771x6x267 counts settle_iterations 244809 game_ticks 244553 due_events 18755499
SIM_WORK sweep states 256 world 3771x6x267 counts dirty_origins 18736509 active_dust 373307440 changed_dust 156674210
SIM_WORK sweep states 256 world 3771x6x267 counts topology_rebuilds 0 topology_rebuild_worker_ns 0
SIM_WORK sweep states 256 world 3771x6x267 counts topology_cells 373307440 topology_probes 328279678
SIM_WORK sweep states 256 world 3771x6x267 counts torch_predicates 82500633 repeater_predicates 1475463843
SIM_WORK sweep states 256 world 3771x6x267 counts comparator_predicates 0 lamp_predicates 1958472
SIM_WORK sweep states 256 world 3771x6x267 scan_worker_ns torch 15385414900 repeater 165464345400 comparator 33381500 lamp 574651400
PHASE exhaustive 48009
WORK exhaustive_vectors 256
```

Gated wall context, **not** retention evidence: `PHASE certify 55960`,
`PHASE manifest 5976`, `CIRCUIT ... in 72.0029272s`.

The four spans sum to the total exactly:
1,732,661,900 + 2,128,300 + 498,619,097,100 + 94,780,200 = **500,448,667,500**.

### 12.3 Same-unit shares on the retained revision

Denominator is `vector_total_worker_ns` = 500,448,667,500 over 256 vectors.

| Rank | Span | Sum (ns) | Share |
|---:|---|---:|---:|
| 1 | settle remainder (dust recompute + due-event application + queue) | 317,161,303,900 | **63.3754%** |
| 2 | repeater mismatch scan | 165,464,345,400 | **33.0632%** |
| 3 | torch mismatch scan | 15,385,414,900 | 3.0743% |
| 4 | clone | 1,732,661,900 | 0.3462% |
| 5 | lamp mismatch scan | 574,651,400 | 0.1148% |
| 6 | check | 94,780,200 | 0.0189% |
| 7 | comparator mismatch scan | 33,381,500 | 0.0067% |
| 8 | drive | 2,128,300 | 0.0004% |
| 9 | dust topology rebuild | 0 | 0.0000% |

Rows 2, 3, 5, 7 and 9 are nested inside `settle`, which is 99.6344% overall. The
four component scans together are **181,457,793,200 ns = 36.2590%** of vector CPU
and 36.3921% of settle.

### 12.4 Change against Task 1's unretained baseline

| Quantity | Task 1 (`225d3e2`) | Retained Layer 1A | Change |
|---|---:|---:|---:|
| vector CPU (worker-ns) | 1,505,590,764,800 | 500,448,667,500 | **−66.7606%** |
| `dirty_origins` | 189,745,183 | 18,736,509 | **−90.1254%** |
| `active_dust` / `topology_cells` | 920,978,899 | 373,307,440 | **−59.4662%** |
| `topology_probes` | 842,804,206 | 328,279,678 | **−61.0491%** |
| settle worker-ns | 1,503,420,849,000 | 498,619,097,100 | −66.8344% |
| component scan worker-ns | 224,275,328,100 | 181,457,793,200 | −19.0915% |
| component scan **share** | 14.8962% | **36.2590%** | +21.36 pp |

The −66.76% CPU reduction is the same order as, and directionally consistent
with, the −61.87% top-exhaustive **wall** reduction measured under the clean gate
in section 11.6. Per spec section 5.1, only the wall figure is retention
evidence; the CPU figure ranks remaining costs.

### 12.5 The logical work is bit-identical

Every count that measures *logical* simulation work is unchanged from Task 1:

| Counter | Task 1 | Retained Layer 1A | Identical |
|---|---:|---:|---|
| `settle_iterations` | 244,809 | 244,809 | yes |
| `game_ticks` | 244,553 | 244,553 | yes |
| `due_events` | 18,755,499 | 18,755,499 | yes |
| `changed_dust` | 156,674,210 | 156,674,210 | yes |
| `torch_predicates` | 82,500,633 | 82,500,633 | yes |
| `repeater_predicates` | 1,475,463,843 | 1,475,463,843 | yes |
| `comparator_predicates` | 0 | 0 | yes |
| `lamp_predicates` | 1,958,472 | 1,958,472 | yes |
| `topology_rebuilds` | 0 | 0 | yes |

This is the strongest semantic evidence produced for Layer 1A so far. The
simulator ran the same number of settle iterations and game ticks, processed the
same scheduled events, changed the same number of dust cells and examined the
same number of component predicates. Only the *redundant re-selection* vanished:
`dirty_origins`, `active_dust` and `topology_probes` collapsed while
`changed_dust` did not move by a single cell.

It also means the −19.09% drop in absolute scan worker-ns is **not** fewer scans.
The scans examined exactly the same predicates; the wall cost per predicate fell,
most plausibly from reduced memory pressure now that whole components are no
longer re-solved between scans. That mechanism is an inference, not a
measurement, and nothing in this report depends on it.

### 12.6 Task 1's open concern 2 is resolved

Task 1 could not explain a residual: `dirty_origins` − `changed_dust` =
33,070,973 exceeded `due_events` 18,755,499 — the upper bound on component
writes — by 14,315,474, so the assumption that the leftover dirt was exactly the
derived write-back could not be confirmed from aggregates.

On the retained revision `dirty_origins` is **18,736,509**, which is now *below*
`due_events` 18,755,499 by 18,990. With the derived write-back discarded, the
remaining dirty origins fit inside the component-write bound exactly as the
theory predicted. Concern 2 is closed by measurement, in addition to the
structural argument and the TDD oracle already recorded in section 11.2.

### 12.7 Spec section 5.5 outcome

Component scans are **36.2590%** of vector CPU, far above the 10% materiality
bar, and their absolute cost of 181,457,793,200 worker-ns is now the largest
single identified span after the unsubdivided settle remainder. **Task 3 is
eligible.**

Two constraints travel with that eligibility:

1. **Layer 1B's own premise is still unconfirmed.** `due_events` / `game_ticks`
   is 76.6930 events per tick, unchanged from Task 1, so ticks are not empty on
   average. The aggregate cannot resolve the per-tick distribution, and Layer 1B
   removes work only on ticks that process zero events. Task 1's open concern 3
   stands and Layer 1B must earn its own 10% wall gate.
2. **The 63.3754% settle remainder is still one unsubdivided span.** No timer
   separates the remaining dust recompute from due-event application and queue
   work. It is now smaller than before in absolute terms but still the largest
   single row; if Layer 1B misses its gate, that span must be split before
   another candidate is chosen.

`dust topology rebuild` remains 0.0000%, so spec section 1's hypothesis 2 is
still refuted. `clone` at 0.3462% and `check` at 0.0189% remain three and four
orders of magnitude below the bar, so no per-worker reset, undo-journal or
snapshot design is warranted.

### 12.8 Instrumentation removal, verified

The three instrumented files were restored to the retained `6ccb7a7` with
`git checkout 6ccb7a7 -- <three paths>`, which preserves the Layer 1A production
change and its oracle because instrumentation was the only difference.

```
git diff 6ccb7a7 -- src              -> empty (byte-identical)

grep -rnE "REDA_SIM_WORK_COUNTS|VectorWork|SimulatorWorkCounts|worker_ns|SIM_WORK|baseline_build_wall_ns" src
SEARCH_EXIT=1                        (no output, no matches)

cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 988 filtered out; finished in 0.12s
EXIT=0

git diff --check          -> exit 0, no output
git diff --cached --check -> exit 0, no output
```

26 rather than 27 tests, because the temporary `VectorWork` arithmetic test was
removed with the type it covered. Because `git diff 6ccb7a7 -- src` is empty, all
of `6ccb7a7`'s test results — the Layer 1A oracle, the narrow differential
commands and the mandatory ignored release suite — carry over unchanged without
being rerun.

The temporary instrumentation now exists only in local history, at `e797853` and
`8b7fe10`. Both are removed by the branch cleanup/squash named in the ledger's
Task 1 split ruling; no counter, gate or disabled branch is in the working
source.

## 13. Layer 1B (Task 3): skip mismatch scans on empty due-event ticks — **REVERT**

### 13.1 Decision

**REVERT.** Layer 1B missed its retention gate. The gate requires `multiplier4`
top-exhaustive wall time at least 10% below the immediately preceding retained
revision `115c976` (source-equivalent to Layer 1A `6ccb7a7`). Measured:

- `median(B) = 46,691 ms`, `median(C) = 42,623 ms`;
- ratio `median(B)/median(C) = 1.095441x`, a **8.7126%** reduction;
- KEEP threshold `0.90 x 46,691 = 42,021.9 ms`; the candidate median is
  **42,623 ms**, missing it by **601.1 ms**.

The plan's KEEP condition is a conjunction of three clauses. Clauses two and
three hold — both representative medians are far below their fixed ceilings, and
every semantic key is byte-identical across all eighteen samples — but clause one
fails, so the candidate is removed. A partial win is not retained; the spec's
ladder measures each candidate independently and deletes the ones that miss.

Everything Layer 1B added was therefore removed: the loop reshape in both stable
APIs, the `#[cfg(test)] component_scan_rounds` field with its initializer,
accessor and increment, the shared ring fixture, and all eight Layer 1B tests.
`src/redstone/simulator/mod.rs` is byte-identical to `115c976` again; see
section 13.8. Sections 13.2 and 13.3 below describe what the candidate *was* and
are retained as the record of a rejected experiment, not as a description of the
current source.

The Task 1 and Task 2 attribution in sections 5, 6 and 12 remains the only
evidence that *attempting* this candidate was justified: on the retained Layer 1A
revision the four component scans are 36.2590% of top-sweep vector worker-ns,
which satisfied the plan's precondition. That precondition is about ranking, not
retention — and this result is a concrete demonstration of the difference. A
36.26% same-unit worker share bought 8.71% of wall time on the objective, so the
share was a ceiling on plausibility, never a prediction.

### 13.2 The change, as it was (now removed)

`src/redstone/simulator/mod.rs`, both stable APIs, minimum loop reshape only:

- `run_until_stable` and `run_until_stable_bounded` each call
  `settle_from_current_state()` exactly once **before** entering their loop. That
  first settle stays unconditional, because the caller may have edited the world
  through `world_mut()` and a pure-dust circuit has no component that could
  detect the edit through the queue.
- Inside each loop the post-advance settle is now conditional on the existing
  `work_done` delta around the advance:

```rust
let work_before = self.work_done;
self.advance_one_tick();
game_ticks_run += 1;
if self.work_done != work_before {
    self.settle_from_current_state();
}
```

- The bounded path uses the same ordering, and returns `WorkLimitExceeded`
  immediately when `advance_one_tick_bounded` refuses an oversized due bucket:
  no `game_ticks_run` increment, no observer sample, no settle after that error.
- `step()` is unchanged, because an external caller may edit the world between
  calls.
- No abstraction, type, trait, clock, scheduler or dependency was added, and no
  production field was added.

Why the skipped scan cannot change behaviour: a game tick on which no scheduled
event was due cannot change world state from inside the simulator, and every
mismatch predicate is a pure function of the world. Any mismatch the previous
scan found is therefore still exactly the same mismatch, and it is already in the
queue — `TickQueue::schedule` accepts at most one entry per position, so
rescanning could only re-derive the identical queue. The `work_done` delta, not a
changed-cell count, is the decision variable precisely because a due event may be
a no-op through burnout or repeater locking and still requires the next scan.

Empty ticks are unchanged in every other respect: they still advance
`current_tick`, still consume the game-tick budget, and still sample an attached
observer, because all of that lives in `advance_one_tick`, which is untouched.

### 13.3 Test-only instrumentation, as it was (now removed)

`Simulator` carries a `#[cfg(test)] component_scan_rounds: u64` field, initialized
to zero, incremented once at the top of `settle_from_current_state`, and read
through a private `#[cfg(test)] fn component_scan_rounds(&self)`. The field and
the accessor do not exist in production builds, so the retained hot path gains no
counter and no disabled branch. `Simulator::new` does not settle, so a freshly
constructed simulator reports zero rounds.

### 13.4 RED/GREEN and the pre-change differential

Step 2's RED command failed on the intended assertion, with the pre-change loop
performing nine scan rounds where two suffice:

```
cargo test --lib redstone::simulator::tests::empty_delay_ticks_do_not_repeat_component_scans -- --exact --nocapture
assertion `left == right` failed  ...  left: 9   right: 2
test result: FAILED. 0 passed; 1 failed
```

After the Step 3 reshape the same command passes.

Step 4 added seven exact semantic regression cases, one per boundary the plan
names (its second bullet names two distinct boundaries — burnout expiry and a
locked repeater — which are two tests). They reuse the existing `torch`,
`wall_torch`, `repeater`, `lamp`, `lever`, `dust`, `stone`, `attach_observer`,
`observations` and `TickQueue` fixtures; the three-wall-torch ring the previous
oscillator test built inline became a shared `ring_of_three_wall_torches()`
fixture in the same test module.

The approved review's test cleanup was then applied, touching no production
logic: the pre-existing `an_oscillator_is_reported_as_diverged`, which only
matched the `Diverged` variant, was deleted because
`an_oscillator_reports_its_exact_diverged_payload` strictly subsumes it (its
surviving rationale moved onto the subsuming test); a redundant
"changes after tick 60 == 21" assertion was dropped from the burnout case, whose
first-burnout/later-recovery replay already proves expiry; an implied
`current_tick == 16` assertion was dropped from the observer case, where the two
`Ok(8)` results and the pinned observation ticks already fix it; and the
bounded-refusal case now watches the two torch positions the allowed due events
actually flip, so its empty-log assertion is genuinely discriminating.

These seven cases were run against the pre-change loops with the counter and
tests kept in place. Every semantic assertion — `Ok`/`Err` payloads,
`current_tick`, `work_done`, final world cells, stale dust power and the observer
log — passed identically before the change. The only pre-change failures were the
`component_scan_rounds` assertions, which is exactly what Layer 1B changes. The
values pinned by those tests are therefore the recorded pre-change behaviour, not
values invented after the fact.

### 13.5 Step 5 gates, run serially

```
cargo test --lib redstone::simulator::tests -- --nocapture
test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 993 filtered out

cargo test --test simulator_circuits -- --nocapture
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 995 filtered out
```

28 simulator tests = 21 pre-existing, minus the subsumed oscillator test, plus
the RED test and the seven Step 4 cases. These are the counts after the review
cleanup; the run above is the final one, made after the last edit to the source.

The certification suite includes the exact cap and lowest-failing-mask tests
(`exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count`,
`certified_candidate_is_identical_at_one_two_and_four_workers`,
`parallel_manifest_sweep_matches_serial_results`), all passing.

Not run against the candidate, and therefore never claimed for it: the ignored
wide full-resettle release suite, the 1/2/4-worker extra/large/hierarchical
corpora, `cargo clippy --all-targets --all-features` and `check.sh`. Those gates
were unnecessary once Step 6 rejected the candidate on wall time.

### 13.6 Step 6 benchmark protocol as executed

The clean `B1,C1,C2,B2,B3,C3` three-circuit protocol from spec section 5.1, with
phase-only binaries — no `REDA_SIM_WORK_COUNTS` build exists on either arm, and
no `SIM_WORK` or `worker_ns` line appears in any of the eighteen logs.

| | B arm | C arm |
|---|---|---|
| Revision | `115c976` (retained Layer 1A) | `12bd58e` (Layer 1B candidate) |
| Location | `$baselineRepo` under `Push-Location` | the active worktree |
| `CARGO_TARGET_DIR` | `%TEMP%\reda-target-layer1a-baseline` | `%TEMP%\reda-target-layer1b-candidate` |
| Executable path in log | `...\reda-target-layer1a-baseline\release\deps\reda-181582025761747b.exe` | `...\reda-target-layer1b-candidate\release\deps\reda-181582025761747b.exe` |
| Lib test count implied | `1013 filtered out` + 1 run = 1014 | `1020 filtered out` + 1 run = 1021 |

The implied test counts are an independent cross-check that each arm ran the
intended revision: 1014 is the Layer 1A suite, and 1021 is that suite minus the
one subsumed oscillator test plus the eight Layer 1B tests. Neither arm rebuilt
between samples.

Raw logs: `%TEMP%\reda-layer1b-{multiplier4,ripple_adder8,alu8}-{B1,C1,C2,B2,B3,C3}.txt`,
eighteen files. All eighteen report
`test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured`.

`top exhaustive` is the final `PHASE exhaustive` line before that case's
`CIRCUIT` line, the extraction rule fixed in section 3.

### 13.7 Raw samples and medians, recomputed from the logs

All values below were re-extracted from the eighteen raw logs and the medians
recomputed independently, not copied from the run summary.

`multiplier4` — top exhaustive, milliseconds (**the objective**):

| Sample | B (115c976) | Sample | C (12bd58e) |
|---|---:|---|---:|
| B1 | 46,254 | C1 | 42,545 |
| B2 | 46,691 | C2 | 42,623 |
| B3 | 46,721 | C3 | 42,748 |
| **median** | **46,691** | **median** | **42,623** |

`median(B)/median(C) = 1.095441x`; reduction `(46,691 - 42,623) / 46,691 =
8.7126%`; KEEP needs `median(C) <= 42,021.9`; actual 42,623. **FAIL.**

End-to-end wall time, seconds:

| Circuit | B raw | B median | C raw | C median | median(B)/median(C) | Fixed ceiling | Ceiling met |
|---|---|---:|---|---:|---:|---:|---|
| `multiplier4` | 69.7930199 / 70.7780746 / 70.4469875 | 70.4469875 | 64.6289979 / 64.3277207 / 64.3865282 | 64.3865282 | 1.094126x | — | — |
| `ripple_adder8` | 9.8597617 / 10.003527 / 9.8472536 | 9.8597617 | 8.7306052 / 8.8913983 / 8.7495453 | 8.7495453 | 1.126888x | 15.36352587 | yes |
| `alu8` | 38.1099869 / 37.8764883 / 37.9787956 | 37.9787956 | 29.9239544 / 30.184643 / 29.9162777 | 29.9239544 | 1.269177x | 54.17791974 | yes |

Paired ratios, normalized `B1/C1`, `B2/C2`, `B3/C3` as the spec requires, reported
to expose drift only — retention uses the ratio of medians:

| Circuit | metric | B1/C1 | B2/C2 | B3/C3 |
|---|---|---:|---:|---:|
| `multiplier4` | top exhaustive | 1.087178 | 1.095441 | 1.092940 |
| `multiplier4` | end to end | 1.079903 | 1.100273 | 1.094126 |
| `ripple_adder8` | end to end | 1.129333 | 1.125079 | 1.125459 |
| `alu8` | end to end | 1.273561 | 1.254826 | 1.269503 |

The three `multiplier4` top-exhaustive paired ratios span 1.0872-1.0954. Every
one is below the 1.1111 ratio that a 10% reduction requires, so the failure is
not an artifact of choosing medians over paired ratios: the candidate misses on
every pairing.

**The end-to-end columns are not evidence for Layer 1B, and are not used to
retain it.** Two facts in the same tables rule that out. First, `alu8` and
`ripple_adder8` certify their top modules with `WORK exhaustive_vectors 0`
(section 4), so they barely exercise the code Layer 1B changes — yet they show
the *largest* end-to-end differences, 1.269x and 1.127x. Second, `alu8` top
exhaustive is 449 ms on B against 456 ms on C, i.e. marginally *slower* on the
candidate, while its end-to-end is 1.269x faster. A change that does not move the
phase it targets cannot be the cause of a 1.27x end-to-end move. These
differences belong to something outside the candidate diff — separate prebuilt
binaries in separate target directories, and machine state across a 10-minute
window — and the honest reading is that the two arms are not comparable end to
end at that resolution. The objective gate is the top-exhaustive phase on
`multiplier4`, which is measured on the phase Layer 1B actually touches, and it
failed.

For the same reason no Goal 1 claim is made here. Both arms already sit far below
the 119.9947335 s Goal 1 threshold, but a Goal 1 verdict requires Task 6's
acceptance matrix against the fixed `225d3e2` baseline, not this two-arm gate.

### 13.8 Semantic identity across all eighteen samples

Every sample produced byte-identical semantic output within its circuit:

| Circuit | Quality key, identical in all 6 samples | `exhaustive_vectors` | `exhaustive_workers` | `manifest_transitions` |
|---|---|---|---|---|
| `multiplier4` | `OK gates=337 blocks_compiled=3 ticks=1039 blocks=124948` | 8, 256, 256 | 1, 12, 11 | 56, 32, 32 |
| `ripple_adder8` | `OK gates=200 blocks_compiled=2 ticks=608 blocks=70603` | 8, 0 | 1 | 56, 68 |
| `alu8` | `OK gates=400 blocks_compiled=3 ticks=972 blocks=213833` | 128, 0, 0 | 12 | 28, 44, 76 |

Gates, compiled blocks, ticks, blocks, per-module vector counts, worker counts
and manifest transition counts are identical between the B and C arms in every
case. The candidate was semantically exact; it simply was not fast enough. This
matters for the record: the reason for removal is the wall-time gate alone, and
nothing here suggests the empty-tick skip was unsound.

### 13.9 One arithmetic correction

The run summary handed over with the logs reported the `multiplier4`
top-exhaustive reduction as 8.7122%. Recomputing from the medians gives
`4,068 / 46,691 = 0.0871260`, i.e. **8.7126%**. The stated ratio `1.095442x`
matches to six significant figures (`1.0954415`). The difference is in the fourth
decimal place of a percentage and changes nothing: both values are far below the
10% threshold and both give the same `FAIL`. The recomputed figure is the one
used throughout this section.

### 13.10 Removal, verified

The complete candidate was removed by restoring the one changed source file from
the retained revision:

```
git checkout 115c976 -- src/redstone/simulator/mod.rs

git diff 115c976 -- src              -> empty (byte-identical)
git diff 115c976 --stat -- src       -> no output

grep -rn "component_scan_rounds\|ring_of_three_wall_torches\|empty_delay_ticks" src
                                     -> no matches

cargo test --lib redstone::simulator::tests -- --nocapture
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 993 filtered out

cargo test --test simulator_circuits -- --nocapture
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 988 filtered out

git diff --check          -> clean
```

21 simulator tests is the pre-Layer-1B count, and the restored
`an_oscillator_is_reported_as_diverged` is among them. No loop reshape, no
`cfg(test)` counter, no accessor, no fixture and no Layer 1B test survives in the
working source. The retained stack is Layer 1A only.

## 14. Task 4: closed next-layer decision — **APPROVED TO REUSE existing evidence, no re-run**

Task 4's plan steps ask for a fresh temporary-instrumentation re-attribution of
the retained stack (Steps 1-2) followed by one measured branch decision
(Step 3). Steps 1 and 2 were deliberately satisfied by reuse instead of being
re-executed, because the current retained `src` is byte-identical to Layer 1A
(`6ccb7a7`/`115c976`): Task 3's removal commit `8cf7869` confirms
`git diff 115c976 -- src` is empty and the working tree at HEAD is byte-for-byte
`115c976` (section 13.10, section 13 provenance). Re-inserting the counters and
re-running the three-circuit attribution would measure the exact same source
`task-2-report.md` Step 7 already measured for this revision, so it would only
duplicate existing evidence rather than produce new information.

The detailed measured attribution for this exact retained source already exists
in this document (section 12, "Step 7 phase B: post-retention re-attribution on
the retained Layer 1A"), sourced from `task-2-report.md` Step 7's "Shares on the
retained revision" and "Spec section 5.5 outcome — Task 3 eligible": top sweep
500,448,667,500 worker-ns, down 66.7606% from Task 1, with the four component
scans at 181,457,793,200 worker-ns (36.2590% of the top sweep).

Step 3's branch decision is taken directly from that existing evidence together
with two independent clean end-to-end medians already measured on this
identical retained source, without any new benchmark:

- `73.8856725s` — Task 2's clean wall-time gate, `multiplier4` end-to-end C-arm
  median (section 11, "End-to-end medians and ceilings" /
  `task-2-report.md` Step 5).
- `70.4469875s` — Task 3's clean wall-time gate, `multiplier4` end-to-end B-arm
  median, measured against the detached Layer 1A baseline `115c976` before the
  (later reverted) Layer 1B candidate (section 13.7).

Both medians were measured on byte-identical retained source and both satisfy
`<= 119.9947335 s` (the Goal 1 threshold from the approved spec). **Branch 1 of
Step 3 fires**: the final end-to-end median is already at or under the Goal 1
threshold, so Layer 2 is skipped and the plan continues to Task 6. Consistent
with section 13.7's caveat, this two-arm evidence establishes that both medians
already sit far below the threshold, but it is not itself a Goal 1 verdict —
**Task 6's acceptance matrix against the fixed `225d3e2` baseline remains the
authoritative Goal 1 verification.**

No `src` file was edited, no `cargo` command was run and no benchmark was
launched for Task 4; this section is a report-only closure of the Step 3
decision from evidence already on record for the identical retained source.
