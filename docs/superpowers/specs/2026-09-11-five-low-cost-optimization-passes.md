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

- Baseline HEAD is `6c9f8b50cb00759a07c28f839c9180dac69696aa`.
- No dependency, GPU, machine-specific tuning, or fixture-specific production
  branch.
- Cargo is **strictly serialized**: exactly one Cargo command runs at a time.
- Every command is **hard-capped at 10 minutes**. A command that hits the cap is
  a failed measurement, never a passing gate.
- Each candidate gets its own cheap gate. A missed opportunity signal or a
  missed threshold means **full revert/removal before the next candidate
  starts**.
- The expensive validation -- four-circuit harness, pinned IO, worker 1/2/4 and
  the flat control -- runs **once, after all retained candidates**, never per
  candidate.
- Preserve semantics, first-error order, the ordered proposal stream, its
  fingerprints and traces, exact certification and manifests, pinned IO, and
  worker determinism.
- TDD evidence must capture a real expected RED before production
  implementation. A test that was green before the production change is
  characterization, and must be labelled as such.
- The task implementer writes the full report to the SDD report file and commits
  its task. No subagents.
- Ponytail full: smallest diff, reuse existing helpers, no speculative
  abstraction.

## Measured baseline (recorded at `6c9f8b5`)

| Signal | Value |
| --- | --- |
| Worktree / branch | clean, `claude/topology-aware-seed-v2-6f8f7e` |
| Reuse fixture runtime | 1.98 s warm; 75.197 s cold command |
| Merge correctness gate | 18 tests, 0.277 s |
| Pull-X focused gate | 2 passed / 1 ignored, 0.672 s |
| Refresh focused gate | 9 passed / 2 ignored, 0.728 s |
| Ripple budget-0 wall | 9.536 s and 9.790 s |
| Ripple budget-0 top `PHASE manifest` | 3.609 s and 3.749 s |
| Ripple budget-0 quality | settle 608, blocks 70603, volume 1123332, static 678 |
| Ripple budget-0 fingerprints | the two strings below |

The two recorded fingerprints, which every retained candidate must reproduce
byte-for-byte:

```text
case      = b9ab139aa9726703f7d5f0d7ed30d50c6a8e0c8b1e2bb4156024a179844573
candidate = a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2
```

These two strings are transcribed from the prior measurement, not re-derived
here. The plan's first task re-captures the same budget-0 transcript and asserts
both strings appear in it verbatim. If the transcript disagrees, the transcript
wins: the implementer records the transcript's values as the authoritative
baseline, notes the correction in the report, and every later gate compares
against the corrected values. No candidate may be measured against an unverified
fingerprint.

The two ripple samples above are the first two of the three-sample median the
retention gates use; the plan captures a third baseline sample before any
production change so every median is over three samples.

## Candidate 1: palette-indexed `BlockFlags` memo

`World` (`src/redstone/world/storage.rs:31-76`) stores one `u32` palette index
per cell. Every simulator neighbour query re-derives flags from the interned
`BlockState`: `connectivity.rs:29` (`is_conductive`), `connectivity.rs:39`
(`supports_dust_step`) and `propagate.rs:435` (`block_signal_at`) all call
`flags_of(world.get(..))`, which walks `BlockKind`, name and half on every hit.
The palette is small and the flags are a pure function of the interned state, so
the derivation is memoizable per palette index.

Contract:

- `World` gains one private `Vec<BlockFlags>` parallel to the palette entries,
  extended exactly where the palette is extended (`set`'s `intern`,
  `from_parts`), and one accessor `World::flags_at(x, y, z) -> BlockFlags`.
- `flags_at` out of bounds returns the **memoized air flags**, read from the
  memo at `air_index`. It must never return a hardcoded `BlockFlags::NONE`;
  `get` already returns air out of bounds and the two must not diverge.
- `dust_topology_changed` (`storage.rs:24`) keeps its existing kind/half/name
  fast path and its `flags_of` call on `BlockState`. It compares two
  `BlockState`s, not two world cells, and the memo does not apply to it.
- Only the three call sites above switch to `flags_at`. `coupling.rs:596`,
  `equivalence.rs:739`, `physical.rs:452,468`, `macro_cells.rs:368` and
  `mod.rs:6055,6285,6595,6748` are not hot-path neighbour queries and stay on
  `flags_of`.

Retention gate. **Direct measured phase improvement, not call count.** Using the
same budget-0 ripple command as the baseline: median top `PHASE manifest` must
be at least **1.5x faster** than the baseline median, and median end-to-end wall
must not regress by more than **5%**, with exact agreement on all four quality
fields, both `WORK` lines, and both fingerprints. Anything less is NO-GO and the
whole candidate is reverted.

## Candidate 2: `prunable_parent_routes` sidecar

`union_and_certify` (`hierarchy_api.rs:449-499`) calls `prunable_parent_routes`
(`hierarchy_api.rs:504`) on every compile, cloning and probing every planned
parent route. The result depends only on `planned.candidate.routes`, which is
owned by the `Arc<PlannedParent>` a reused plan hands back unchanged.

This is a re-proposal of the reverted `14aead2` ("perf: reuse hierarchical
compile invariants", reverted by `95b6b9d`). That revert's scope is prohibited:
no `ModuleCompileContext`, no flattening hoist, no restructuring of
`union_and_certify`'s call graph.

Contract, and it is conditional:

- **Diagnostics first.** Add two permanent `PHASE` lines beside the existing
  `PHASE union`: `PHASE flatten` around `module_flattening`
  (`hierarchy_api.rs:452`) and `PHASE prunable` around `prunable_parent_routes`
  (`hierarchy_api.rs:454`), both under the existing `REDA_PHASE_TIMING` guard
  and in the existing `eprintln!` `PHASE name millis` shape. These are the one
  sanctioned permanent survivor, justified by the existing diagnostics they
  join.
- **Kill switch.** If the median `PHASE prunable` over the measured runs is
  `<= 5 ms`, the cache is NO-GO: it is not implemented at all, the two `PHASE`
  lines stay (they are the evidence), and the wave moves on.
- If and only if median `PHASE prunable > 5 ms`, add a **sidecar** memo tied
  structurally to the exact `Arc<PlannedParent>`: a one-entry cache validated by
  `Arc::ptr_eq` against the stored `Arc`, so a recycled allocation cannot alias.
  It is roughly 15 lines, lives beside `prunable_parent_routes`, holds the `Arc`
  itself plus the `BTreeSet<RouteId>`, and is consulted only when `Arc::ptr_eq`
  holds. It changes no signature that is not private to `hierarchy_api.rs`.

Retention gate: the `PHASE prunable` median must exceed 5 ms to start, and the
sidecar must then produce a measurable drop in that same line with exact quality
and fingerprint agreement. Otherwise NO-GO and removed.

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
- A crate-visible consumer index is built once per batch and threaded into a new
  private helper that `merge_isolation_mask` also calls, so the two cannot
  drift. The public function keeps building its own index when called alone.
- **First-error order is preserved.** Each batch caller still visits gates in
  ascending gate index and returns the first error it meets, with the same error
  value it returns today. Building the index earlier must not surface an error
  that today's first failing gate would have pre-empted.

Retention gate: a **disposable** repeated probe over a real `InstanceGraph`
/`expand` batch (not a synthetic netlist), removed before the candidate commit.
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
  into a repeater carrying the standing refresh's own state with the proven
  successor facing, then runs the existing `prune_route` on the mutated tree.
- Legality is validated **for every branch containing the inserted cell**, using
  the existing `branches_carry_through` walk. A branch that does not contain the
  cell is unaffected by construction and is not re-proven.
- Enumeration is deterministic: `Reverse(maximum path depth)` then `Anchor`, the
  same total order `prune_route` and `relocate_refresh` already use. `Anchor`'s
  derived `Ord` is the only tiebreak, and nothing reads map iteration order,
  worker count or wall clock.
- **At most one candidate cell is tried per standing refresh.** The fallback is
  a bounded escape hatch, not a second search.
- On no gain, **full cell state is snapshotted and restored**: the inserted
  cell's prior state and every state `prune_route` changed. The tree must be
  byte-equal to its input after a refused fallback.
- A step is retained only when the route's repeater count **strictly falls**.
  Inserting one and pruning one is no gain and is undone.
- `non_air_blocks` and `occupied_volume` stay invariant: dust and repeater both
  occupy one cell, and no cell is added or removed. The `JointQuality` guard
  from the 2026-09-10 spec continues to be the only acceptance rule for this
  stage.

Retention gate: a focused fixture that pruning and straight relocation both
provably refuse, plus at least one accepted refresh-stage proposal on a real
acceptance circuit whose gain comes from the fallback. Otherwise NO-GO and
removed.

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
  fingerprints, both `WORK` lines, pinned IO placements and contracts, and
  worker determinism at 1/2/4.
- The wave's outcome is "N of 5 retained" for whatever N the measurements give.

## Evidence rules

- Every gate number in the report is a measured value with its command, its
  sample count and its cap outcome. No projected, extrapolated or
  asymptotically-argued number is a gate.
- Every median is over three samples unless the value is a pass/fail count.
- A disposable probe is named as disposable in the report and is proven removed
  by the candidate's own commit.
- The SDD report file for this wave is
  `docs/superpowers/reports/2026-09-11-five-low-cost-optimization-passes.md`.
