# Five Low-Cost Optimization Passes, Evaluated as One Wave

Date: 2026-09-11. Base: HEAD `6c9f8b5`
(`6c9f8b50cb00759a07c28f839c9180dac69696aa`), branch
`claude/topology-aware-seed-v2-6f8f7e`, clean worktree. Every source anchor
below is as of that commit.

This document specifies **one evaluation wave of exactly five low-cost
candidates**. It does not promise that five passes land. Each candidate is
measured against its own cheap gate, and a candidate that misses its
opportunity signal or its threshold is fully removed before the next candidate
starts. A wave that retains zero candidates is a legitimate outcome of this
spec, recorded as five attempted evaluations.

## Relationship to the 2026-09-10 spec

`docs/superpowers/specs/2026-09-10-pareto-tick-density-passes.md` remains
binding except for exactly two claims, which this document **supersedes**:

1. "There is no sixth stage." Superseded. A sixth stream stage (Pull-X round 2)
   may exist if and only if it earns retention under this document's gates.
2. "Relocation is straight-only", i.e. the rule that a refresh mutation may only
   rewrite an identical straight horizontal `P -> U -> D` segment and may never
   insert a cell. Superseded for the Refresh Relocation stage only, by the
   insert-then-prune fallback below.

Everything else in that document stays in force verbatim: the `JointQuality`
acceptance policy and its density non-regression guard, Passes 1-4 keeping
`Acceptance::Lexicographic`, budget-zero byte-identity, one evaluation per
offered descriptor, deterministic trace prefixes, at-most-once stage freezing,
the refusal catalogue, the rebuild order, and every deferred item. In
particular, **bend-aware relocation and sibling merge are not separate passes**:
they are one route rewrite, and this document implements that rewrite as the
Refresh Relocation fallback rather than as new stages.

## Goals

- Evaluate five independent low-cost candidates against measured gates, and
  retain only the measured winners.
- Keep every retained candidate semantics-preserving: identical certified
  quality, identical work counts, identical fingerprints.
- Leave the repository in a state where a NO-GO candidate is indistinguishable
  from never having been attempted, except in the written report.

## Non-goals

- No new dependency, no GPU path, no machine-specific tuning, no
  fixture-specific production branch.
- No density algorithm, no Cell Topology Search, no module-rebuild cache, no
  `ModuleCompileContext`, no flattening hoist. The prunable candidate below is a
  re-proposal of the reverted `14aead2` / `95b6b9d` and is deliberately narrower
  than that revert's scope.
- No public API change, no configuration knob, no serialized trace field, no new
  `QualityKey` field.
- No temporary counter, timer, feature flag or disabled branch surviving the
  wave. The single exception is a permanent `PHASE` line, and only where the
  existing `REDA_PHASE_TIMING` diagnostics in
  `src/compile/fragment_synth/certification.rs:273-377`,
  `src/compile/fragment_synth/seed.rs:659-797` and
  `src/compile/fragment_synth/hierarchy_api.rs:479-481` justify it.
- No repository-wide formatting or lint cleanup inside a candidate commit.

## Global constraints

These hold for every task in the implementation plan.

- The source baseline is `6c9f8b50cb00759a07c28f839c9180dac69696aa`. `HEAD` may
  be a later docs commit; what is required is that the baseline is an ancestor,
  the worktree is clean, and `git diff --exit-code 6c9f8b5 HEAD -- src` is empty.
- No dependency, GPU, machine-specific tuning, or fixture-specific production
  branch.
- Cargo is **strictly serialized**: exactly one Cargo command runs at a time.
- Every command is **hard-capped at 10 minutes**. A command that hits the cap is
  a failed measurement, never a passing gate.
- The whole execution wave is budgeted at **60 minutes** of measured command
  time, so many sub-10-minute commands cannot accumulate into another multi-hour
  run.
- Each candidate gets its own cheap gate. A missed opportunity signal or a
  missed threshold means **full revert/removal before the next candidate
  starts**.
- The final validation -- four-circuit semantic check at budget 0, pinned IO and
  worker 1/2/4 at one evaluation -- runs **once, after all retained candidates**,
  never per candidate, and stays inside the same caps. Exhaustion (`u64::MAX`) is
  never run for `multiplier4` or for all four circuits; `multiplier4` exhaustion
  is measured at over 68 minutes. The ~40-minute flat acceptance harness does not
  fit a 600 s cap and is explicitly deferred or replaced by an existing bounded
  flat control, with the ruling and residual risk written down.
- Preserve semantics, first-error order, the ordered proposal stream, its
  fingerprints and traces, exact certification and manifests, pinned IO, and
  worker determinism.
- TDD evidence must capture a real expected RED before production
  implementation. A test that was green before the production change is
  characterization, and must be labelled as such.
- Each task's implementer writes the full report to the SDD report file and
  commits its task. The controller dispatches a fresh implementer and reviewer
  per task; those agents do not spawn nested agents.
- Ponytail full: smallest diff, reuse existing helpers, no speculative
  abstraction.

## Measured baseline (recorded at `6c9f8b5`)

| Signal | Value |
| --- | --- |
| Worktree / branch | clean, `claude/topology-aware-seed-v2-6f8f7e` |
| Reuse fixture runtime | 1.98 s warm; 75.197 s cold command |
| Merge correctness gate | 18 tests total across its two commands, 0.277 s |
| Pull-X focused gate | 2 passed / 1 ignored, 0.672 s |
| Refresh focused gate | 9 passed / 2 ignored, 0.728 s |
| Ripple budget-0 paired sample 1 | max `wall_ms` 9789; the pair was 9.536 s and 9.790 s |
| Ripple budget-0 sample 1 top `PHASE manifest` | max 3749 ms; the pair was 3.609 s and 3.749 s |
| Ripple budget-0 quality | settle 608, blocks 70603, volume 1123332, static 678 |
| Ripple budget-0 fingerprints | the two strings below |

The two recorded fingerprints, which every retained candidate must reproduce
byte-for-byte:

```text
case      = b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573
candidate = a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2
```

Both strings are valid and are preserved exactly; the plan's first task asserts
both appear verbatim in a freshly captured transcript before any candidate is
measured.

The budget-0 harness certifies each case at two budget points, so one capped
command compiles the case **twice**. One command is therefore one **paired
sample**, read as the maximum top `PHASE manifest` and the maximum `RETENTION`
`wall_ms` within it -- never as two independent samples. The recorded pair above
is sample 1; the plan captures two more commands so every median is over three
paired samples.

## Candidate 1: palette-indexed `BlockFlags` memo

`World` (`src/redstone/world/storage.rs:31-76`) stores one `u32` palette index
per cell. Every simulator neighbour query re-derives flags from the interned
`BlockState`: `connectivity.rs:29` (`is_conductive`), `connectivity.rs:39`
(`supports_dust_step`) and `propagate.rs:435` (`block_signal_at`) all call
`flags_of(world.get(..))`, which walks `BlockKind`, name and half on every hit.
The palette is small and the flags are a pure function of the interned state, so
the derivation is memoizable per palette index.

Contract:

- **`Palette` owns the memo**, as one private `Vec<BlockFlags>` parallel to its
  `entries`, pushed in `Palette::intern` -- the only mutator -- and exposed as
  `Palette::flags(index) -> Option<BlockFlags>`, the same shape `get` uses. The
  parallel-vector invariant then has exactly one place it can be violated.
- `World::flags_at(x, y, z) -> BlockFlags` **delegates**: the cell's palette
  index in bounds, `air_index` out of bounds -- the same fallback `get` already
  uses. `World` holds no second vector and restates no invariant. An optional
  `debug_assert_eq!` against `flags_of(self.get(..))` is allowed only if it earns
  its place.
- Out of bounds therefore returns the interned air entry's flags, which is what
  the in-bounds air cell returns. Air's flags legitimately **are**
  `BlockFlags::NONE`; the contract is agreement with `get`, not inequality with
  `NONE`, and no test may assert the latter.
- `dust_topology_changed` (`storage.rs:24`) keeps its existing kind/half/name
  fast path and its `flags_of` call on `BlockState`. It compares two
  `BlockState`s, not two world cells, and the memo does not apply to it.
- Only the three call sites above switch to `flags_at`. `coupling.rs:596`,
  `equivalence.rs:739`, `physical.rs:452,468`, `macro_cells.rs:368` and
  `mod.rs:6055,6285,6595,6748` are not hot-path neighbour queries and stay on
  `flags_of`.

Retention gate. **Direct measured phase improvement, not call count.** Using the
same budget-0 ripple command as the baseline, over three paired samples: median
top `PHASE manifest` must be at least **1.5x faster** than the baseline median,
and median `RETENTION` `wall_ms` must not regress by more than **5%**, with exact
agreement on all four quality fields, the ordered `WORK` sequence, and both
fingerprints. Anything less is NO-GO and the whole candidate is reverted.

## Candidate 2: `prunable_parent_routes` sidecar

`union_and_certify` (`hierarchy_api.rs:450-501`) calls `prunable_parent_routes`
(called at `:464`, defined at `:503`) on every compile, cloning and probing every
planned parent route. The result depends only on `planned.candidate.routes`,
which is owned by the `Arc<PlannedParent>` a reused plan hands back unchanged.

This is a re-proposal of the reverted `14aead2` ("perf: reuse hierarchical
compile invariants", reverted by `95b6b9d`). That revert's scope is prohibited:
no `ModuleCompileContext`, no flattening hoist, no restructuring of
`union_and_certify`'s call graph. Also prohibited: a mutable one-entry cache, an
`Arc::ptr_eq` or `Arc::as_ptr` cache key, and any production probe counter.

Contract, and it is conditional:

- **Diagnostics first.** Add two permanent `PHASE` lines beside the existing
  `PHASE union`: `PHASE flatten` around `module_flattening`
  (`hierarchy_api.rs:462`) and `PHASE prunable` around `prunable_parent_routes`
  (`hierarchy_api.rs:464`), both under the existing `REDA_PHASE_TIMING` guard
  and in the existing `eprintln!` `PHASE name millis` shape. These are the one
  sanctioned permanent survivor, justified by the existing diagnostics they
  join.
- **Measured on the plan-reuse path, not on ripple.** Budget-0 ripple has zero
  plan-reuse hits, so its `PHASE prunable` lines cannot show what this candidate
  removes. The gate is the existing
  `unchanged_block_placements_reuse_the_incumbent_plan` fixture
  (`hierarchy_api.rs:4047`), and the measured quantity is that command's
  aggregate `PHASE prunable` cost.
- **Kill switch.** If the median relevant prunable cost is `<= 5 ms`, the sidecar
  is NO-GO immediately: it is not implemented at all, the two `PHASE` lines stay
  (they are the evidence), and the wave moves on.
- If and only if that median is `> 5 ms`, couple the value **structurally** to
  the plan rather than caching it: a small private
  `RoutedParent { planned: PlannedParent, prunable_routes: BTreeSet<RouteId> }`,
  with `HierarchicalCandidate::planned` becoming `Arc<RoutedParent>`.
  `prunable_routes` is computed at the one `RoutedParent` construction site, so
  the existing `Arc::clone` reuse branch carries it for free and there is nothing
  to invalidate. `prunable_parent_routes` keeps its signature and body.
- The `PHASE prunable` line **moves with the computation** into
  `RoutedParent::new`, under the same guard and the same name, so it stays a
  permanent diagnostic of the thing it names. A compile that reuses a plan then
  emits no `PHASE prunable` line at all; that absence is the win, not a lost
  measurement.

Retention gate: the median relevant prunable cost must exceed 5 ms to start, and
the sidecar must then produce a measurable drop in it, with the fixture's own
assertions still passing. The TDD claim is `Arc<RoutedParent>` identity across a
reuse, and equal `prunable_routes`. Otherwise NO-GO and removed.

## Candidate 3: hoisted merge consumer index

`merge_isolation_mask` (`topology.rs:555-587`) rebuilds a
`HashMap<&str, Vec<usize>>` over **every gate and every gate input** on each
call. Its batch callers call it once per merge gate: `instantiate_gates`
(`instance_graph.rs:824`), `expand_with_selection` (`primitive_graph.rs:613`)
and `shared_merge_branches` (`primitive_graph.rs:507`). The batch cost is
therefore quadratic in gate count where a single shared index would be linear.

Contract:

- `merge_isolation_mask`'s public signature, return type, `MergeMaskError`
  variants and **error precedence** are unchanged: unknown gate, then not a
  merge, then too many inputs, evaluated in exactly that order, before any index
  is consulted.
- No new type. `build_consumer_index(&Netlist) -> HashMap<&str, Vec<usize>>` is
  today's loop lifted verbatim, and
  `merge_isolation_mask_with_index(&Netlist, GateIndex, &HashMap<..>)` is today's
  body below it. The public `merge_isolation_mask` builds the index and delegates,
  so the two paths cannot drift. Batch callers build it once before their loop.
- **First-error order is preserved.** Each batch caller still visits gates in
  ascending gate index and returns the first error it meets, with the same error
  value it returns today. Building the index earlier must not surface an error
  that today's first failing gate would have pre-empted.
- The correctness gate is exactly these two commands, 18 tests in total at the
  measured baseline, and no looser filter may be substituted:
  `cargo test --release --lib merge_isolation -- --nocapture --test-threads=1`
  and
  `cargo test --release --lib compile::fragment_synth::instance_graph::tests -- --nocapture --test-threads=1`.

Retention gate: a **disposable** repeated probe over a real `InstanceGraph`
/`expand` batch (not a synthetic netlist), removed before the candidate commit.
`build_seven_segment_netlist` and `Library::default_library()` are both present
at the source baseline, so the probe uses the real seven-segment netlist.
Retention requires **both** at least **1.5x** faster median targeted batch time
**and** a nontrivial absolute saving of at least **100 ms** across the probe's
20 repeats. Correctness alone does not qualify. Asymptotic argument alone does
not qualify.

## Candidate 4: filtered second bounded Pull-X round

`pull_x_edge` (`hierarchy_api.rs:962-981`) freezes Pull-X descriptors once, from
the incumbent standing when the alignment stage is exhausted, keeping only edges
where `block_pull_x_proposal` returns `Some`. After round 1 has moved blocks,
some edges that were filtered out -- or that were offered and are now one cell
closer -- can again offer a legal single-cell pull, and the frozen vector can
never say so.

Contract:

- One new stage, **Pull-X round 2**, placed **immediately after Pull-X round 1
  and before the seam, prune and refresh stages**. Stage index arithmetic
  extends the existing chain in the same shape and forces round 1's vector to
  freeze before its length is read.
- Round 2 is frozen once, lazily, on first reach, from the then-current
  incumbent. An edge is offered **only if** the incumbent's sink `dx` is
  non-zero **and** `block_pull_x_proposal` returns `Some` for it. Both
  conditions, together; either alone offers edges the union cannot use.
- Round 2 carries `Acceptance::Lexicographic`, exactly like round 1. It is a
  placement pass, not a refresh pass.
- Fingerprint schemas are exactly `hierarchical-block-pull-x2-fragment-v1` and
  `hierarchical-block-pull-x2-choice-v1`. Round 1's schemas are untouched.
- A stale descriptor is refused, never retargeted; budget zero still freezes
  nothing.

Cheap pre-gate, and its limit is part of the contract: **the pre-gate is
plan-only**, because the route structure a real gain depends on does not exist
until the proposal is replanned and re-routed. The pre-gate therefore only
answers "does round 2 offer any descriptor at all on a real circuit". If it
offers none, the candidate is NO-GO immediately. If it offers some, **at most
one** capped certified ripple run decides retention: retention requires at least
one accepted round-2 proposal that strictly improves `QualityKey`. Otherwise
NO-GO and removed.

## Candidate 5: insert-then-prune fallback for Refresh Relocation

`relocate_refresh` (`route_opt.rs:96-281`) today only rewrites an identical,
straight, horizontal `U..=D` window: `straight` (`route_opt.rs:203-208`) refuses
any bend or vertical step, and every trial only ever **moves** `U`'s repeater
into the window. A route whose refreshes sit across a bend, and a sibling pair
that a single new repeater would let pruning collapse, are both refused today.
Extending this one function subsumes both "bend-aware relocation" and "sibling
merge"; they are one route rewrite and get no stages of their own.

Contract:

- The fallback runs **only** when the existing straight relocation retained
  nothing for a standing refresh, inside `relocate_refresh`, on the same
  renumbered parent clone, before any child is stamped.
- It inserts exactly one repeater by converting **one route-owned dust cell**
  into a repeater carrying the standing refresh's exact `BlockState`, then runs
  the existing `prune_route` on the mutated tree. No facing is reconstructed.
- Legality is validated **for every branch containing the inserted cell** in two
  layers: the existing `route_step_is_legal(previous, at, next, state)` must
  accept the same inserted state on every affected branch, then the existing
  `branches_carry_through` walk must prove signal strength. A branch that does
  not contain the cell is unaffected by construction and is not re-proven.
- Enumeration is deterministic: `Reverse(maximum path depth)` then `Anchor`, the
  same total order `prune_route` and `relocate_refresh` already use. `Anchor`'s
  derived `Ord` is the only tiebreak, and nothing reads map iteration order,
  worker count or wall clock.
- **Exactly one candidate cell is tried per standing refresh: the
  furthest-downstream legal one.** When that sole attempt fails, the tree is
  restored and the pass moves to the **next standing refresh** -- never to a
  second cell for the same refresh. The fallback is a bounded escape hatch, not a
  second search.
- On no gain, **full cell state is snapshotted and restored**: the inserted
  cell's prior state and every state `prune_route` changed. The tree must be
  byte-equal to its input after a refused fallback.
- A step is retained only when the route's repeater count **strictly falls**.
  Inserting one and pruning one is no gain and is undone.
- `non_air_blocks` and `occupied_volume` stay invariant: dust and repeater both
  occupy one cell, and no cell is added or removed. The `JointQuality` guard
  from the 2026-09-10 spec continues to be the only acceptance rule for this
  stage.

Retention gate: a focused, contiguous bent fixture that first passes
`RealisedRouteTree::validate` and that pruning and straight relocation both
provably refuse, plus at least one accepted refresh-stage proposal on a real
acceptance circuit whose gain is explicitly attributed to the fallback. The
attribution uses an internal relocation outcome and the existing choice
fingerprint; it does not add a trace field, schema change, counter or feature
flag. Otherwise the candidate is NO-GO and removed.

## Retention, revert and NO-GO policy

- Candidates are evaluated in the order 1, 2, 3, 4, 5. Each finishes -- retained
  or removed -- before the next begins.
- A NO-GO candidate is recorded in the report as **attempted**, with its
  measurement, and is then fully removed: no dead code, no feature flag, no
  disabled branch, no leftover test, no leftover fixture. `git diff` against the
  candidate's start commit must be empty except for the report, and for
  Candidate 2's two sanctioned `PHASE` lines.
- Retained candidates must each independently preserve: all four `QualityKey`
  fields, `evaluations_used`, `stop_reason`, every trace entry, both
  fingerprints, the ordered `WORK` sequence, pinned IO placements and contracts,
  and worker determinism at 1/2/4 (measured at one evaluation, not exhaustion).
- The wave's outcome is "N of 5 retained" for whatever N the measurements give.

## Evidence rules

- Every gate number in the report is a measured value with its command, its
  sample count and its cap outcome. No projected, extrapolated or
  asymptotically-argued number is a gate.
- Every median is over three capped commands unless the value is a pass/fail
  count. Two compiles inside one command are one paired sample, not two.
- The running total of measured command time is recorded against the 60-minute
  wave budget as the wave proceeds.
- A disposable probe is named as disposable in the report and is proven removed
  by the candidate's own commit.
- The SDD report file for this wave is
  `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.
