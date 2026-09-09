# Portable Incremental Simulator — Layer 0 attribution

Spec: `docs/superpowers/specs/2026-09-09-portable-incremental-simulator.md` (Revision 2, approved 2026-09-09)
Plan: `docs/superpowers/plans/2026-09-09-portable-incremental-simulator.md`
Baseline commit: `225d3e28c41616d5f975b740df60d1352f4512a7` (`225d3e2`).

This report records the Task 1 / Layer 0 attribution only. The temporary
instrumentation that produced it was removed in the same commit that adds this
file; no counter, gate or disabled branch survives in the simulator hot path.

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
