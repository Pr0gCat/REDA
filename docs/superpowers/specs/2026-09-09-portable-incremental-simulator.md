# Portable Incremental Simulator

Status: written specification approved by the user on 2026-09-09. Revision 2
incorporates the adversarial source and measurement reviews.

## 1. Problem and evidence

At commit `225d3e2`, REDA already distributes exhaustive vectors through the
existing deterministic certification workers. The remaining cost is inside
each vector: clone one pristine `Simulator`, drive the canonical mask, settle
the physical world, and check the outputs. Adding another executor would only
nested-parallelize work that already owns the machine.

The current machine was measured again after the architecture discussion with
a prebuilt release profile, `REDA_PHASE_TIMING=1`, no certification overrides,
and one Cargo process at a time:

| Circuit | End to end | Relevant simulator work | Quality |
|---|---:|---:|---|
| `multiplier4` sample 1 | 184.2595029 s | top exhaustive 145.296 s; manifest 12.491 s | 1,039 ticks / 124,948 blocks |
| `multiplier4` sample 2 | 179.9921003 s | top exhaustive 141.387 s; manifest 12.356 s | 1,039 ticks / 124,948 blocks |
| `multiplier4` sample 3 | 178.2080015 s | top exhaustive 139.193 s; manifest 12.513 s | 1,039 ticks / 124,948 blocks |
| `ripple_adder8` | 14.0882662 / 14.6319294 / 15.0606173 s; median 14.6319294 s | top manifest 7.743 / 8.225 / 8.868 s | 608 ticks / 70,603 blocks |
| `alu8` | 51.5980188 / 51.7810176 / 50.6753885 s; median 51.5980188 s | top manifest 24.966 / 24.650 / 23.340 s | 972 ticks / 213,833 blocks |

The authoritative same-session `multiplier4` medians are therefore
179.9921003 seconds end to end and 141.387 seconds for top exhaustive. During
the top exhaustive phase, fifteen one-second samples observed 9.72--10.70 CPU
cores in use on a 12-logical-core host, 13 process threads, and a 430.7--437.9
MiB working set. This is CPU work, not an idle or I/O-bound phase.

The same clean `225d3e2` binary now also supplies three-sample medians for the
two regression controls. Their fixed 5% ceilings are 15.36352587 seconds for
`ripple_adder8` and 54.17791974 seconds for `alu8`.

Source inspection identifies four repeated costs that need attribution before
retention:

1. every exhaustive mask deep-clones `World` and the mutable simulator state;
2. a caller input changes `Air` to `RedstoneBlock`, invalidating the shared
   dust topology cache and rebuilding the complete dust topology for every
   exhaustive mask containing at least one high pinned input;
3. dust fixed-point write-back uses ordinary `World::set`, marking derived dust
   writes dirty. `Simulator::new` leaves that whole write-back in the pristine
   baseline, so all 256 clones begin with the same whole-component no-op dust
   recomputation;
4. every settle iteration scans all torches, repeaters, comparators and lamps,
   including game ticks on which no scheduled event is due.

The design removes proven duplicate work first. It does not weaken the physical
simulation, reduce vector counts, or infer that logical equivalence substitutes
for whole-world behavior.

## 2. Goals

1. Improve the same-session `multiplier4` end-to-end median by at least 1.5x,
   from 179.9921003 seconds to at most 119.9947335 seconds.
2. Keep `ripple_adder8` and `alu8` end-to-end medians at or below their fixed
   same-session ceilings of 15.36352587 and 54.17791974 seconds respectively.
3. Preserve exactly: candidate and emitted-world fingerprints, `QualityKey`,
   pinned IO coordinates, proposal traces, transition measurements, cap work,
   typed error payload and lowest failing logical index.
4. Produce identical results at fixed certification worker counts 1, 2 and 4.
5. Keep the complete production implementation portable Rust using the existing
   standard-library worker architecture. One-core execution remains complete.
6. Obtain clone, drive, settle, output-check, dust and component-scan work
   attribution before choosing the next conditional layer.
7. Retain only general improvements that pass their measurement gate. If the
   1.5x end-to-end target is outside the measured removable fraction, retain the
   single best passing general improvement and report the upper-bound evidence.

## 3. Non-goals

- No Gray-code, nearest-vector, prefix-tree or other settled state reuse across
  exhaustive masks.
- No bit-sliced second simulator, SIMD lane policy or GPU path in this project.
- No change to canonical mask order, exhaustive threshold, vector count,
  manifest contents, transition count or any cap.
- No new thread pool, executor, dependency, platform-specific tuning or
  fixture-name branch.
- No change to tick priority, component scheduling order, stable insertion
  order or per-kind YZX order.
- No density, redstone latency, placement or routing optimization.
- No snapshot/rollback implementation unless the recorded Layer 0 attribution
  proves cloning is still material after the simpler layers.

## 4. Semantic invariants

### 4.1 Independent exhaustive vectors

Every mask starts from the exact pristine baseline with `work_done == 0`, an
empty baseline queue and baseline burnout history. `enforce_event_cap` continues
to measure from zero. A successful or failed mask cannot affect another mask.

`run_indexed_chunks` remains the only worker partition and reduction mechanism.
All workers finish, then results reduce in logical chunk and mask order so the
lowest failing mask still wins independently of completion order.

### 4.2 Tick and scheduling order

The production component order remains:

1. torch;
2. repeater;
3. comparator;
4. lamp.

Within each kind, candidates are examined in increasing flat world index, the
existing YZX order. The existing torch sub-order is also preserved: all standing
`Torch` positions precede all `WallTorch` positions. `TickQueue` stable insertion
behavior is unchanged. An optimization may avoid evaluating a component only
when it proves that both the component's mismatch predicate and relevant queue
eligibility cannot have changed. A component skipped because it is locked or
already scheduled has not established that proof and remains a candidate.

### 4.3 Caps and observations

`work_done` continues to count only processed scheduled events. The game-tick
cap is checked at the same boundary. Empty ticks still advance time and sample
an attached observer; only redundant dust and mismatch evaluation may be
skipped. Burnout expiry, locked repeaters and no-op due events remain observable
and must cause the same later scheduling decisions.

Retained phase diagnostics remain enabled by `REDA_PHASE_TIMING`. Layer 0's
detailed counters additionally require `REDA_SIM_WORK_COUNTS`, print only to
stderr, and exist only in the temporary attribution revision. They are removed
before any production optimization is benchmarked or retained, so no counter
field or disabled branch remains in the final simulator hot path.

## 5. Chosen architecture: measured optimization ladder

Each candidate is implemented and measured independently. A candidate that
misses its gate is removed before another candidate is attempted. Later
candidates use the post-retention revision as their baseline, but the final
decision is always against clean `225d3e2`.

The minimum individual gates do not imply Goal 1: `0.9 * 0.9 * 0.8 = 0.648`
would reduce top exhaustive by 35.2%, while the measured end-to-end target
requires at least a 42.4349% top-exhaustive reduction if every other phase is
unchanged. Passing every individual gate but missing 1.5x therefore triggers a
new attribution decision; it is not evidence of a physical limit.

### 5.1 Layer 0: temporary coarse attribution

Use a temporary local diff around the existing diagnostics rather than add a
profiler, dependency or permanent instrumentation API. Detailed attribution is
active only when both `REDA_PHASE_TIMING` and `REDA_SIM_WORK_COUNTS` are set.
It records one outer wall span for pristine baseline construction and, per
exhaustive vector, four worker-duration spans:

- `baseline.clone()`;
- input drive;
- settle;
- logical evaluation and output check.

Per-vector durations are reduced by the existing indexed worker reduction into
sum, maximum and count. Their sum is explicitly labelled worker-nanoseconds,
not wall time; it may explain relative CPU work but is never divided into a
phase wall time or used as an Amdahl bound. Only the existing `PHASE exhaustive`
and `CIRCUIT ... in ...` intervals are wall-time retention evidence.

Simulator-local counters use already-computed lengths and record deltas after
the pristine baseline so cloned construction work is not multiplied into every
vector. They report:

- settle iterations and game ticks;
- due scheduled events, identical to the existing work counter delta;
- dust dirty origins, active and changed dust positions;
- topology rebuild count, rebuild worker-nanoseconds, dust cells visited and
  connection probes;
- torch, repeater, comparator and lamp scan worker-nanoseconds and predicates
  examined.

No shared atomic is touched per vector. Aggregates print only after deterministic
reduction. After the attribution report identifies the dominant removable work,
all Layer 0 code and `REDA_SIM_WORK_COUNTS` are removed. Production candidates
are then benchmarked from a clean phase-only binary, so diagnostics cannot hide
permanent disabled-path overhead.

The reproducible benchmark command is:

```powershell
$test = 'compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan'
$env:REDA_PHASE_TIMING = '1'
Remove-Item Env:REDA_CERT_THREADS -ErrorAction SilentlyContinue
Remove-Item Env:REDA_CERT_MEMORY_BYTES -ErrorAction SilentlyContinue
Remove-Item Env:REDA_SIM_WORK_COUNTS -ErrorAction SilentlyContinue
$env:CARGO_TARGET_DIR = '<baseline-or-candidate-target>'
$env:REDA_EXTRA_CIRCUITS = '<one circuit name>'
cargo test --release --lib $test -- --exact --ignored --nocapture --test-threads=1
```

The prebuilt baseline and candidate executables live in separate target
directories and run in alternating `B,C,C,B,B,C` order. Each reported median
therefore has three samples without rebuilding between samples. The run sequence
is labelled `B1,C1,C2,B2,B3,C3`; paired diagnostic ratios are always normalized
as `B1/C1`, `B2/C2`, `B3/C3` regardless of execution order. Retention uses the
objective's ratio of medians, `median(B) / median(C)`, not the median of paired
ratios. Paired ratios are reported only to expose drift. Detailed lines include
state count and world dimensions so the unique top exhaustive sweep is
identified without plumbing fixture or module names through production APIs.
Baseline construction and vector sweep have separate labels.

In every clean phase-only hierarchical benchmark, `top exhaustive` means the
final `PHASE exhaustive` line before that case's `CIRCUIT ...` line. This is
the top module because `compile_hierarchical_scoped` finishes child blocks
before its final `compile_module_with_blocks` call for `lowered.top`, and the
benchmark uses budget zero. The immediately following
`WORK exhaustive_vectors` line is an extraction cross-check, not module
identity.

### 5.2 Layer 1A: discard cached recompute's derived dust dirty set

`recompute_active_dust` computes a fixed point inside every selected weak dust
component before writing results. Changed wire powers are returned immediately
to the component scheduler. The cached recompute has already consumed all
pre-existing dirty origins before write-back, and the function holds the only
mutable borrow of `World`. Therefore any dirty entries left immediately after
write-back are exactly the derived wire-power writes.

Keep ordinary `World::set`; after the cached recompute returns its changed
positions, call the existing `World::take_dirty()` once to discard those derived
entries. Do not add a special writer or change the public
`recompute_dust_strengths` path. External input writes, scheduled component
writes, public propagation behavior, palette/index maintenance and topology
epochs remain on the existing path.

This optimization depends on the selected active set being complete. The small
TDD oracle verifies the cached path returns the same changed positions and final
world while leaving no derived dirty entries. Before retention, all existing
wide full-resettle differential harnesses -- six condition circuits, negotiated
plans, the 240 `and4s` transitions, isolation worlds and injected isolation
worlds -- must pass. Those tests deliberately remove the old self-dirty safety
net and are mandatory, not optional measurement fixtures.

Retention gate: at least 10% reduction in `multiplier4` top-exhaustive wall time,
with the preceding attribution naming the removed work, no semantic difference
and no representative end-to-end regression above 5%. A counter-only win is
removed.

### 5.3 Layer 1B: skip mismatch scans on empty due-event ticks

`run_until_stable` and `run_until_stable_bounded` perform one complete initial
settle after the caller's possible external edits. They then advance game ticks
exactly as today. After an advance, they repeat dust and component scheduling
only if at least one scheduled event was processed on that tick.

The decision uses the existing `work_done` delta around `advance_one_tick` or
`advance_one_tick_bounded`, not the number of changed cells: a due event may be
a no-op because of burnout or repeater locking and still requires the next
scan. An empty due-event tick cannot change world state internally, but it still
advances `current_tick`, consumes the same game-tick budget and samples the
observer. Public `step()` remains unchanged because an external caller may edit
the world between calls.

TDD covers a delayed repeater with several empty ticks, a no-op due event,
burnout, a locked repeater, divergence at the tick limit and observer sampling.
Every case compares final world, current tick, processed-event count and exact
error payload with the original behavior. The bounded path additionally covers
a due bucket larger than the remaining event allowance and asserts the exact
`WorkLimitExceeded { used, limit }`, queue tick and lack of post-error observer
or dust work.

Retention gate: at least 10% reduction in `multiplier4` top-exhaustive wall time,
with the preceding component-predicate attribution naming the removed work and
the same semantic and regression gates as Layer 1A. A counter-only win is
removed.

### 5.4 Layer 2: dirty-scoped mismatch scheduling

Layer 2 is implemented only if retained earlier candidates do not reach the
final target and component predicates remain a material share of settle work.

Every tracked simulator-internal write changes only `lit` or `power`. It cannot
change the `kind`, `name`, `half` or `facing` fields read by dust connection
shape. A component mismatch predicate can therefore depend on an internal dirty
origin at most two Manhattan hops away: component to support/rear/side, then to
that power query's neighbour. A changed dust `power` value has the same
distance-two bound. Any arbitrary edit that can change connection shape enters
through `world_mut()` and forces the next scan to be full.

A focused invariant test exercises every scheduled component write and dust
write-back and asserts the topology tuple `(kind, name, half, facing)` is
unchanged. Existing bent-run three/four-hop torch shapes instead test the
`world_mut()` full-scan boundary: an external shape edit must never enter the
radius-two scoped path.

The simulator therefore keeps five ordered candidate sets: standing torch,
wall torch, repeater, comparator and lamp. It also keeps two pending `BTreeSet`s
for dirty flat indices and changed-dust flat indices. Every cached recompute
merges its consumed origins and changed dust into those pending sets, including
the recomputes inside `advance_one_tick`, `advance_one_tick_bounded` and
`settle_from_current_state`. Nothing is cleared merely because Layer 1B skips an
empty tick.

`Simulator::new` returns with both pending sets empty and the first scan marked
full; constructor recompute work is therefore not copied into every exhaustive
clone. `world_mut()` likewise clears any old pending sets before marking the next
scan full. While a full scan is already pending, recomputes need not populate the
two sets because that scan examines every component anyway.

For the first settle after construction or external mutable world access, all
components are candidates. Later candidate sets are the union of:

- active components within two hops of every consumed dirty origin;
- active components within two hops of every changed dust position;
- every component whose previous visit did not conclude "examined and matched".

The retained set is mandatory: a mismatched component, a locked repeater, or a
repeater/comparator skipped because it is already scheduled has not proved
itself matched and must be revisited. Candidate construction deduplicates
origins first and enumerates each exact Manhattan ball once with bounded
`dx/dy/dz` loops -- 25 positions at radius two, not a duplicate-producing
recursive frontier. Each position is classified once by its current kind.
Candidate sets use flat indices in `BTreeSet`s, preserving the old YZX order.
The 20% retention gate, rather than another population cache or threshold,
decides whether this scoped construction is cheaper than the existing scan.

The five kind passes retain the old Torch-then-WallTorch-then-repeater-then-
comparator-then-lamp insertion order. Component removal or placement can occur
only through `world_mut()`, so the mandatory full scan handles it without
candidate-set invalidation machinery. Only after all five passes finish are the
pending origin/change sets cleared. `Simulator::world_mut()` marks the next
settle as a full scan before returning its mutable reference. The full scan
leaves both pending sets empty after all passes. This conservative boundary
covers arbitrary external edits and complete `World` replacement without
another identity/epoch protocol.

Only if Layer 2 is reached, the cached dust recompute internal result is extended
to return both the dirty origins it consumed and the dust positions it changed.
This is private simulator data; the public `Vec<Position>` result remains
unchanged. No static component cache or duplicated invalidation policy is added.

TDD runs the scoped and test-only full-scan schedulers tick by tick over normal,
scheduled, locked, burnout, oscillating and topology-changing worlds, including
the existing bent-run three/four-hop torch cases and interleaved standing/wall
torch coordinates. The bent-run cases settle the straight run, edit the bend
through `world_mut()`, then prove that the full-scan path ran. A lamp case covers
an already-scheduled delayed turn-off whose desired state changes back to on.
Every case compares queue contents in insertion order, world cells, current
tick, work counter, observations and exact terminal error after every tick.

Retention gate: at least 20% reduction in remaining top exhaustive time and no
semantic or representative regression failure. The final combined gate remains
1.5x end to end on `multiplier4`.

### 5.5 Closed post-layer decision

After every retained candidate, reapply the same temporary attribution in a
disposable diff, record it, remove it again, and rerun the three-circuit wall
matrix. Choose the next change only from a measured dominant cost:

- if full dust-topology rebuild is at least 10% of aggregate vector
  worker-nanoseconds, write and review a separate local-rebuild design before
  Layer 2;
- if component scans are at least 10% of aggregate vector worker-nanoseconds,
  Layer 2 is eligible;
- if cloning is at least 10% of aggregate vector worker-nanoseconds, write and
  review a separate per-worker reset design. No undo-journal shape is selected
  before that evidence exists;
- otherwise inspect the next measured cost rather than inventing another cache.

For ranking, aggregate vector worker-nanoseconds means the sum of the four
explicit spans -- clone, drive, settle and check -- over all successful vectors.
`bits_of` and `enforce_event_cap` remain uninstrumented residual work and are
not silently included in that denominator. These same-unit worker shares rank candidates only. They never prove a wall-time
gain, trigger retention or contribute directly to an Amdahl/physical bound.
Those decisions require separately measured phase and end-to-end wall times.

The work continues while eliminating measured remaining work would still permit
the 1.5x end-to-end target. A physical-limit claim is valid only when a complete
phase wall decomposition supplies an evidence-backed lower bound for every
remaining phase and their sum still exceeds 119.9947335 seconds. Running out of
candidates, a worker-time share, or exhausting this document's initial layers is
not such evidence. Without measured wall-time lower bounds, the physical-limit
fallback is unavailable and the work continues.

If that strict physical upper bound is below 1.5x, use a clean `225d3e2`
benchmark worktree to replay each passing general optimization independently,
and retain only the one with the largest multiplier4 end-to-end median
improvement that also passes both regression ceilings and every semantic gate.

## 6. Error handling

No new production error category is needed. Instrumentation overflow uses
saturating diagnostic counters and cannot fail certification. Existing
`CounterOverflow`, `TransitionDidNotSettle`, `SimulatorEventCapExceeded`,
`FunctionalMismatch` and worker panic behavior remain untouched.

Discarding the cached recompute's derived dirty entries is permitted only after
the changed positions have been captured and only inside the cached simulator
path. Dirty-scoped candidate construction is deliberately conservative: extra
candidates cost time, while a missing candidate can issue a false certificate
and is therefore forbidden.

## 7. Verification and retention matrix

Every production behavior change follows RED/GREEN TDD, then runs serial Cargo
commands on Windows.

Focused gates:

1. temporary attribution unit/arithmetic checks, recorded raw output and a clean
   final diff proving all detailed instrumentation was removed;
2. cached dust no-self-dirty tests plus every existing narrow and ignored wide
   full-resettle differential;
3. empty-tick, burnout, lock, lamp, divergence, bounded-cap and observer tests;
4. scoped-vs-full tick differential tests if Layer 2 is reached, including
   accumulators across both advance-time and settle-time recomputes, the
   topology-tuple invariant, and the `world_mut()` full-scan boundary;
5. exhaustive event-cap and functional-mismatch lowest-mask tests at workers
   1, 2 and 4;
6. manifest source-group equality and complete certification serial/parallel
   equality.

Acceptance gates:

1. alternating clean-baseline/candidate `ripple_adder8`, `alu8` and
   `multiplier4` release benchmarks, three samples per side from separately
   prebuilt executables;
2. reuse the existing fixed 1/2/4-worker matrices over all 16 extra circuits,
   four large circuits and four hierarchical circuits, plus the existing cap,
   lowest-error and worker-panic precedence cases;
3. exact ticks, blocks, occupied volume, static delay, candidate/world
   fingerprints, proposal traces, cap payloads, transition measurements and
   byte-identical `simulator_revision()`;
4. pinned seven-segment release contract and its eleven literal IO positions;
5. six acceptance cases and the repository's complete `check.sh` gate, including
   root, viewer and wasm tests/build rather than only the library suite;
6. `multiplier4` with `REDA_CERT_THREADS=1` must remain within 5% of its clean
   one-worker baseline, so the optimization is not bought by slowing small
   machines. Baseline and candidate each use three samples in the same
   `B1,C1,C2,B2,B3,C3` order, and the gate is
   `median(candidate) <= 1.05 * median(baseline)`;
7. `cargo clippy --all-targets --all-features` and `git diff --check`;
8. independent correctness review and over-engineering review.

The report records exact commands and environment, raw samples, paired ratios,
medians, wall-time versus worker-time units, CPU/work counters, peak working
set, quality and fingerprints. If final 1.5x is missed, it follows the closed
decision in section 5.5; it never calls worker-time sums or an exhausted idea
list a physical upper bound. Every candidate that misses its own gate is
removed.

## 8. Rejected alternatives

- **More certification workers:** the phase already sustains about ten cores on
  this host and uses the existing memory-aware deterministic budget.
- **Gray code or settled-state chains:** changes history, event accounting and
  worker-dependent arc boundaries.
- **Bit-parallel physical simulator:** creates a second physics implementation
  with per-lane queues, burnout, ticks and caps.
- **Whole-`Vec` copy-on-write:** almost every vector writes an input, so its first
  write still copies the dense world; page overlays also tax every `World::get`.
- **Static component-position cache first:** removes allocation but still tests
  every component on every tick and requires another topology invalidation
  protocol. Layer 2 removes the larger scan directly if measurement requires it,
  while mutable external access simply requests one conservative full scan.
- **Incremental dust topology rebuild without attribution:** component
  merge/split correctness is substantially larger than removing self-dirty and
  empty scans. It receives a separate design only if Layer 0 shows that full
  rebuild is material.
- **Palette/taxonomy caches without a profile:** a previous palette-transition
  cache failed its wall-time gate; no cache is added on call-count intuition.
- **GPU-first:** the current workload is irregular event-driven state with exact
  per-vector error and cap semantics. CPU algorithmic waste is removed first.
