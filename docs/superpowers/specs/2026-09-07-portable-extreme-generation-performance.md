# Portable Extreme Generation Performance

Status: architecture approved in chat on 2026-09-07; this written revision is
pending user review.

## 1. Problem

REDA's hierarchical generator now avoids recompiling repeated modules and the
current worktree reuses an incumbent `PlannedParent` for seam and prune
proposals whose block placements do not change. The remaining generation wall
time is dominated by work below those wins:

- parent routing is serial, and its hot loops repeatedly walk predecessor
  chains and clone complete reservation maps;
- each proposal still unions and certifies a whole flat candidate;
- `finish_attempt` emits and physically verifies a candidate, then
  `CompleteCandidateCertifier` emits and physically verifies it again;
- exhaustive truth certification is serial while the manifest sweep is
  parallel;
- each simulated vector/transition creates a fresh simulator from a cloned
  world, and each settle tick scans broad component sets;
- independent stages can each create their own threads, so adding more
  parallel loops naively would oversubscribe the machine.

One local `ripple_adder8` budget-zero profile measured about 18.6 seconds in
top-level routing and 19.3 seconds in top-level certification. The existing
retention report records roughly 1,000 seconds for one `multiplier4` proposal;
its eight inputs trigger 256 serial exhaustive vectors. These numbers identify
the work, but the design must benefit REDA users on arbitrary CPUs and operating
systems. No processor, core count, GPU vendor, or local benchmark number is an
architectural constant.

## 2. Goals

1. Reduce end-to-end circuit-generation latency on one-core and multi-core
   machines without changing the generated circuit.
2. Keep a portable CPU implementation as the complete production path. It
   detects available parallelism at runtime and remains efficient at one
   thread.
3. Preserve byte-for-byte candidate fingerprints, proposal traces, quality,
   cap accounting, deterministic errors, emitted worlds, pinned IO and every
   certification result for semantics-preserving stages.
4. Preserve full whole-world structural verification, equivalence proof,
   exhaustive truth where configured, manifest simulation and timing analysis.
5. Prevent nested thread oversubscription. A compile owns one worker budget and
   assigns it to the stage that can use it best.
6. Reach at least 2.0x geometric-mean end-to-end speedup on the representative
   corpus, with no representative circuit more than 5% slower. Each retained
   milestone must improve its targeted phase by at least 1.5x.
7. Keep GPU acceleration optional and self-disabling. CPU-only installations
   require no GPU runtime and produce identical results.

## 3. Non-goals

- No reduction in simulation vectors, proof limits or verification rules.
- No compositional substitute for final flat whole-world certification.
- No change to `QualityKey`, pass ordering, acceptance semantics or time-budget
  boundary semantics.
- No routing heuristic, neighbour order or tie-break change in the first router
  performance milestone; route geometry and fingerprints must stay identical.
- No density or redstone settle-delay optimisation in this project.
- No GPU dependency in the CPU milestones, and no GPU implementation without
  the evidence gate in section 9.
- No persistent cross-process cache.

## 4. Invariants

### 4.1 Determinism

Parallel work is partitioned into stable contiguous index ranges. Results are
joined and reduced in increasing logical index, never completion order. The
first reported error is the lowest failing vector, transition, module or
proposal under the existing order. Worker panic propagation is deterministic
under that same order.

For every test case, thread counts `1`, `2` and `auto` must produce the same:

- candidate and case fingerprints;
- emitted-world fingerprint and `CandidateMetrics`;
- proposal trace, terminal classification and accepted incumbent sequence;
- certification measurements and worst-transition indices;
- typed error and cap-work counters on refusal paths.

### 4.2 Certification authority

Every candidate is structurally checked, emitted, physically verified,
equivalence-proved and simulated exactly as required today. Removing duplicate
work means sharing the result of one authoritative execution inside one
certification transaction, not skipping that authority or reusing a result for
a different candidate.

Any reusable value is keyed by all inputs that can affect it. Candidate-local
values cannot enter a global cache. An `Arc` only shares immutable certified or
planned data; mutation remains copy-on-write at explicit proposal boundaries.

### 4.3 Budgets

Evaluation budgets still count committed proposal evaluations. A time budget is
checked at the same boundary as today and may be crossed by the proposal already
in progress. Speculative work, if later enabled, is never counted until committed
and cannot extend the legal committed window.

## 5. Chosen architecture

The optimization is delivered as independently measurable layers. A layer that
misses its retention gate is reverted before the next layer begins.

```text
hierarchical compile
  -> immutable compile context
  -> placement and routing
  -> union
  -> one certification transaction
       -> structure + emission + physical verification
       -> timing + equivalence
       -> exhaustive vectors / manifest transitions
  -> deterministic proposal commit
```

The CPU path uses existing standard-library scoped threads. No general executor
or dependency is introduced until existing primitives are proven insufficient.

### 5.1 Layer 0: complete observability

Extend `REDA_PHASE_TIMING` without changing normal output. Record wall time and
work counts for:

- parent planning, channel layout and each routed net;
- union, relocation and splicing;
- structural verification, emission, physical verification, timing graph,
  equivalence, exhaustive truth, manifest sweep and fingerprints;
- world bytes cloned/reset, reservation entries cloned/overlaid, A* node
  expansions and predecessor steps;
- worker count and active parallel stage.

Timing labels are diagnostic only and never enter a fingerprint. The benchmark
harness consumes structured records or stable prefixed lines; tests do not assert
wall-clock duration.

### 5.2 Layer 1: remove duplicate and invariant work

There is one certification transaction per candidate:

1. validate structure, shape and ownership once;
2. create the adapter and emitted world once;
3. run the durable physical verifier once;
4. seal that exact world with its structural certificate;
5. pass the sealed world to timing, equivalence, simulation and metric assembly.

`finish_attempt` no longer performs a disposable emit/verify preflight before
calling `CompleteCandidateCertifier`. Existing injectable test facades are
collapsed into the single transaction rather than retained as a second
production pass.

Within the transaction, compute the candidate fingerprint, library revision,
transition manifest and manifest fingerprint once and pass them by reference.
For hierarchical search, hoist top-module flattening and paths into the immutable
compile context. Cache `prunable_parent_routes` beside the exact immutable
`PlannedParent` it describes. Rebuilding a parent plan creates a new sidecar.

This layer changes ownership and call signatures only. It cannot change route or
simulation order.

### 5.3 Layer 2: portable coordinated parallel certification

Parallelize `certify_exhaustive_truth` with the same deterministic chunk pattern
as the manifest sweep. Both paths use one shared helper for stable partitioning,
ordered collection, cap handling and panic propagation.

A compilation-wide parallelism policy chooses one active parallel dimension:

- several ready modules: compile modules concurrently and give each module one
  certification worker;
- one active module/candidate: give its exhaustive or manifest sweep the full
  worker budget;
- small work item: run serially when estimated chunks are too small to amortize
  thread startup.

The policy defaults to `available_parallelism`, accepts the existing explicit
thread override for tests and diagnostics, and clamps to at least one. It does
not read CPU model names. The current global manifest lock is removed only when
the compilation-wide ownership rule makes concurrent nested sweeps impossible;
until then it stays as the safety boundary.

The first implementation may continue to use scoped threads. A persistent pool
is considered only if Layer 0 proves thread creation is at least 10% of the
targeted phase after the useful work is optimized.

### 5.4 Layer 3: router asymptotic and copy-cost fixes

Preserve the exact search frontier ordering, neighbour ordering, costs and
tie-breaks. Optimize only equivalent queries and storage:

- replace repeated full predecessor-chain scans in self-obstruction checks with
  path ancestry metadata that answers the same four obstruction predicates
  without walking the complete chain;
- replace per-route and per-channel full `PhysicalReservations` clones with a
  read-through overlay: immutable base reservations plus an ordered local delta;
- build lookup tables once for sink assignments, structural slack and endpoint
  geometry instead of linearly rescanning vectors for each target;
- reuse allocated neighbour and scratch buffers when ownership permits.

The ancestry representation must support predecessor replacement correctly. If
an O(1) representation cannot preserve the exact current search semantics, use
the smallest exact O(log n) representation or reject that sub-change.

The overlay must expose the same ordered reads and commit exactly the cells the
successful route laid. Attempt-local keep-outs never leak into the base. A
debug differential harness runs clone-based and overlay-based requests against
the same inputs and compares the full route tree and failure object.

### 5.5 Layer 4: simulator template and incremental settling

Separate immutable world topology from per-run dynamic state. A
`SimulatorTemplate` may precompute component positions, static connectivity,
observer sites and other data derived solely from the certified world. Creating
a transition simulator still starts from the same initial block state and keeps
independent queue, burnout, work and observer state.

Then replace full component scans per tick with an ordered dirty frontier. A
changed input, applied scheduled event or changed conductor adds exactly the
components whose rules can observe that change. Burnout torches and locked
repeaters remain live until their existing rules say otherwise. Dust propagation
uses its current semantics; it is changed only behind a tick-by-tick differential
oracle.

The old full-scan implementation remains available under tests as the oracle
until the complete simulator differential corpus passes. Production switches to
incremental settling only after every compared tick has identical world state,
queue state, observations, work count, stable tick and error.

### 5.6 Layer 5: proposal scheduling, only if still material

The greedy proposal stream is incumbent-dependent, so it remains serial by
default. If Layer 0 still attributes at least 20% of end-to-end time to
non-certification proposal work, add bounded lookahead inside one frozen stream
stage:

- evaluate a small ordered window against the same incumbent;
- commit strictly in proposal order;
- after the first accepted result, discard every later speculative result;
- never cross an unfrozen Pull-X or prune stage boundary;
- never exceed the remaining evaluation budget.

Lookahead is retained only when its end-to-end gain includes wasted work and
peak memory. It must not compete with an exhaustive or manifest sweep for the
same worker budget.

## 6. Memory model

Performance cannot come from retaining complete candidates indefinitely.

- Immutable compile context and one incumbent plan may be shared with `Arc`.
- A worker owns its candidate, simulator state and overlay delta.
- Ordered result slots retain only what commit or error reduction needs.
- Speculative proposal count is bounded by the worker budget and a measured
  memory ceiling.
- Benchmarks record peak working set. A layer is rejected if it improves time by
  moving the bottleneck to unbounded memory growth.

## 7. Error handling and fallback

- Allocation, worker panic and typed certification failures preserve existing
  error categories; no failure becomes a silent serial retry unless the current
  API already permits that retry.
- `auto` parallelism falling back to one worker is supported behavior.
- Unsupported GPU, driver failure or an unprofitable GPU size threshold falls
  back to the CPU before candidate evaluation begins.
- Incremental-simulator mismatches fail tests and block production activation;
  they do not fall back per transition and hide a correctness defect.

## 8. Benchmark and retention gates

Benchmarks measure compiler generation wall time, not redstone settle ticks.
Each point runs after build, records at least three samples and reports the
median plus the full sample list. Cold build time is separate.

Representative circuits:

| Case | Purpose |
|---|---|
| `ripple_adder8` budget 0 and exhaustion | routing/certification balance and proposal reuse |
| `multiplier4` budget 0 and one retained pass | exhaustive-certification worst case |
| `alu8` budget 0 | three-level hierarchy and large union |
| six acceptance cases | stable public quality/fingerprint baseline |
| 16 extra and four large circuits | generality and regression coverage |
| pinned seven segment | literal 11-pin contract |

Every representative point runs at thread counts `1`, `2` and `auto` where the
stage supports parallelism. CI or release evidence must include at least two
distinct machines before claiming cross-machine scaling; correctness never
depends on that availability.

Retention requires all of:

1. exact semantic invariants from section 4;
2. targeted phase speedup at least 1.5x for the milestone;
3. final representative-corpus geometric mean at least 2.0x;
4. no representative case more than 5% slower at the same thread setting;
5. bounded peak memory reported and accepted;
6. complete debug, release, ignored acceptance and pinned-IO verification.

An optimization that fails its gate is removed, not hidden behind a default-on
flag. A useful but hardware-specific experiment may remain an off-by-default
benchmark prototype outside the production path.

## 9. Optional GPU evidence gate

GPU work starts only after the CPU layers are measured. It requires one isolated
kernel that is:

- at least 30% of remaining end-to-end wall time;
- batchable without changing event order or cap accounting;
- representable without copying the complete world for every small step;
- portable through one maintained cross-vendor API;
- at least 2.0x faster end to end, including transfer and setup, on two GPU
  vendors while never slowing CPU-only runs.

The likely candidate is batched independent transition simulation, not A*
routing. A GPU result must match the CPU oracle bit-for-bit. If any gate fails,
REDA ships the optimized CPU path only.

## 10. Test strategy

Every production change follows RED/GREEN TDD:

- duplicate-work tests count authority calls and first fail at two;
- deterministic partition tests inject success, typed failure and panic at
  different indices and assert lowest-index reduction;
- exhaustive serial/parallel tests compare complete certification outputs;
- router ancestry and reservation overlays run differential requests against the
  current implementation and compare exact route/failure values;
- simulator tests compare every tick, not only final outputs;
- thread-count tests compare all fingerprints, traces, metrics and cap work;
- benchmark harnesses do not contain correctness assertions based on time.

Each layer lands separately with its before/after report. Full verification is
rerun after the final layer rather than inferred from focused tests.

## 11. Delivery order

1. Layer 0 instrumentation and frozen baselines.
2. Layer 1 duplicate/invariant work removal.
3. Layer 2 exhaustive certification and worker-budget coordination.
4. Layer 3 router asymptotic/copy fixes, one independent sub-change at a time.
5. Layer 4 simulator template, then dirty-frontier settling behind the oracle.
6. Re-profile; implement Layer 5 only if its 20% gate is met.
7. Evaluate the GPU gate; ship no GPU code if it fails.
8. Run the full cross-thread and representative-circuit acceptance matrix.

## 12. Rejected alternatives

- **GPU-first:** excludes CPU-only users and attacks an unproven kernel before
  removing algorithmic waste.
- **Parallelize every loop:** oversubscribes cores and makes latency worse when
  certification already owns all workers.
- **Parallel greedy proposals without invalidation:** changes incumbent-dependent
  proposal contents, traces and results.
- **Change A* queue/tie-break for speed:** may change routing geometry and
  candidate fingerprints; this project first exhausts equivalent data-structure
  improvements.
- **Incremental certification by affected region:** unsafe until a complete
  dependency proof exists; whole-world certification remains authoritative.
- **Large persistent cache:** trades time for stale-key and memory risks without
  addressing the underlying repeated work.
