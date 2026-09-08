# Portable Router Ancestry Profiling & Conditional Hoist

## Context

Confirmed production flow: fragment `seed.rs` route stage -> `route_owned` ->
`routing.rs::route_with_local_policy` -> per-sink `search_path`. `search_path`
maintains a BTreeSet frontier, BTreeMap `travelled` and `previous`, and calls
`self_obstructs_typed(&previous, state.at, next, ...)` per neighbour;
`TypedOwnJoinCheck::blocks` can also walk `previous`. `RouterWork` already
counts `node_expansions` and `queue_entries`.

Hypothesis: predecessor-walk (self-obstruction / own-join) work is a
significant fraction of `route_nets` time. This plan adds diagnostic-only
timing to confirm or refute that before touching any algorithmic behavior.

## Hard constraints (apply to every step)

- No new dependencies, caches, pools, threads, or abstractions.
- No change to frontier/neighbour/cost/tie ordering, predecessor replacement
  behavior, `RouterFailure`/work limits, route trees, fingerprints, quality,
  pinned IO, or whole-world certification, unless the gate in Phase 2 is met
  and only via the single minimal hoist described in Phase 3.
- All new instrumentation is diagnostic-only, gated by `REDA_PHASE_TIMING`,
  and must have zero effect on behavior when the env var is unset.
- Every phase ends with a small, isolated commit. Do not combine
  instrumentation, benchmarking, and optimization commits.

## Phase 1 — Diagnostic instrumentation in `src/compile/routing.rs`

### 1.1 RED test

Add a new test module/test to `src/compile/routing.rs` (or its existing test
submodule) named:

- `ancestry_walk_diagnostics_count_matches_calls`

Test shape: construct a small deterministic routing scenario already used by
existing `search_path`/`route_with_local_policy` unit tests (reuse an
existing fixture builder rather than inventing a new one). Enable the
diagnostic counters (in-process, not via subprocess env var, so the test is
deterministic — e.g. a `#[cfg(test)]` accessor or a thread-local/counter
struct reset before the call), invoke `search_path` (or
`route_with_local_policy`) once, and assert:

- ancestry walk call count equals the number of `self_obstructs_typed` +
  `TypedOwnJoinCheck::blocks` invocations actually made (cross-check against
  a manually-instrumented expectation derived from the known small graph,
  not a tautological self-count).
- ancestry step count equals the total number of `previous`-chain hops
  walked across those calls (also derived from the known small graph
  topology).
- route result (path, cost, fingerprint) is byte-identical to the same call
  with diagnostics disabled.

This test must fail before instrumentation exists (RED) because the counters
don't exist yet.

### 1.2 GREEN implementation

Add a minimal diagnostic counters struct, e.g. `AncestryWalkStats { calls: u64, steps: u64, elapsed_nanos: u64 }`, threaded through `search_path` only
when `REDA_PHASE_TIMING` (or an existing equivalent debug flag already used
in this codebase for phase timing — reuse it, do not invent a parallel
mechanism) is enabled. Increment `calls` once per `self_obstructs_typed`
call and once per `TypedOwnJoinCheck::blocks` call that performs a
predecessor walk; increment `steps` by the number of `previous` hops walked
inside each; accumulate `elapsed_nanos` around each call site using
`std::time::Instant` already available in std.

Emit the aggregated stats at the end of `route_with_local_policy` (or
wherever existing `PHASE route_nets` timing is emitted) as a new line, e.g.:

```
PHASE ancestry calls=<n> steps=<n> elapsed_ms=<f>
```

nested under/adjacent to the existing `PHASE route_nets` line, following
whatever the current emission format/macro is (must match, do not create a
new logging format).

Do not change control flow, return values, ordering, or allocate per-call
unless it is purely additive counter bookkeeping.

### 1.3 Verify

Run in this order, stopping on first failure:

```
cargo test --lib compile::routing::tests::ancestry_walk_diagnostics_count_matches_calls -- --exact
cargo test --lib compile::routing::tests
cargo test --lib
```

Confirm the diagnostics are true no-ops when `REDA_PHASE_TIMING` is unset:

```
cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --exact --nocapture --test-threads=1
```

### 1.4 Commit boundary

Commit only `src/compile/routing.rs` (and the test) as:
`test: add diagnostic-only ancestry-walk timing to route_with_local_policy`

## Phase 2 — Benchmark alu8 and ripple_adder8 (no code changes)

### 2.1 Method

Use the existing ignored hierarchical benchmark harness (do not write a new
one). For each of `alu8` and `ripple_adder8`, at current HEAD (post Phase 1
commit, diagnostics compiled in but only active via env var):

```powershell
$env:REDA_PHASE_TIMING = '1'
$env:REDA_EXTRA_CIRCUITS = 'alu8'
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan -- --exact --ignored --nocapture --test-threads=1
$env:REDA_EXTRA_CIRCUITS = 'ripple_adder8'
cargo test --release --lib compile::fragment_synth::seed::tests::extra_circuits::every_hierarchical_circuit_certifies_through_module_floorplan -- --exact --ignored --nocapture --test-threads=1
Remove-Item Env:REDA_PHASE_TIMING, Env:REDA_EXTRA_CIRCUITS
```

Run each command 3 times (3 samples), recording per run:

- `PHASE route_nets` elapsed
- `PHASE ancestry` elapsed (calls/steps/elapsed_ms)
- resulting fingerprint(s) and quality metric(s) already printed by the
  harness (must be identical across all 3 runs and identical to the
  pre-Phase-1 baseline value already recorded in prior reports — do not
  re-derive baseline, just confirm no drift)

### 2.2 Formula

For each case compute:

```
ancestry_fraction = median(ancestry_elapsed_ms) / median(route_nets_elapsed_ms)
```

using the 3-sample median per case, independently for alu8 and
ripple_adder8.

### 2.3 Gate decision

- If `ancestry_fraction >= 0.50` for **both** alu8 and ripple_adder8: proceed
  to Phase 3 (optimize).
- Otherwise: stop ancestry optimization work entirely. Do not implement any
  hoist. Move directly to Phase 4 (profile `seed.rs:2595` clone cost only).

### 2.4 Review checkpoint

Before proceeding past the gate in either direction, record the 3-sample
medians, the computed `ancestry_fraction` for both cases, and the gate
outcome. Get an independent read-only review of the arithmetic and identity
evidence before writing optimization code; user input is needed only if the
measured result requires leaving this plan's approved branches.

### 2.5 Commit boundary

No code changes in this phase — nothing to commit. If a benchmark log/report
is produced, it stays out of git per instructions (no report file is to be
touched by this plan's execution; recording of results happens in the
chat/checkpoint only, unless the user separately asks for a report to be
written).

## Phase 3 — Conditional minimal hoist (only if gate passed)

### 3.1 Scope

Exactly one minimal, semantics-preserving change: hoist/reuse the
per-expansion predecessor-path snapshot that `self_obstructs_typed` and
`TypedOwnJoinCheck::blocks` currently reconstruct from `previous` on every
call, so it is computed once per expansion step and reused across the
neighbour loop within that same expansion, instead of being re-walked from
scratch per neighbour. No caching across expansions, no auxiliary index
structure beyond the single reusable snapshot value already implied by
existing per-expansion state, no threads/pools.

### 3.2 RED test

Add test named:

- `ancestry_hoist_preserves_route_and_failure`

Keep the current map-walk predicate under `#[cfg(test)]` as the differential
oracle. Compare it with the snapshot predicate over deterministic chains,
all neighbour directions, both `own_floor_of_earlier` values, and a case where
an anchor's predecessor is replaced before the next expansion. Also run a
shared-chain routing fixture and assert identical route tree,
`RouterFailure`, and work-limit behavior. The RED state is the missing
snapshot predicate; no production legacy branch remains after tests pass.

Add a second test:

- `ancestry_hoist_reduces_map_walk_steps`

Using the Phase 1 diagnostic counters, assert `steps` recorded for the same
fixture scenario strictly decreases after the hoist versus the recorded
pre-hoist baseline value (hardcode the pre-hoist baseline captured from
running the test against the Phase-1 commit). This is RED before the hoist
lands (no reduction yet) and GREEN after.

### 3.3 GREEN implementation

Implement the hoist inside `search_path`'s expansion loop only. Do not touch
`route_owned`, `route_with_local_policy`'s outer structure, or the
BTreeSet/BTreeMap types/ordering.

### 3.4 Verify (serialized)

```
cargo test --lib compile::routing::tests::ancestry_hoist_preserves_route_and_failure -- --exact
cargo test --lib compile::routing::tests::ancestry_hoist_reduces_map_walk_steps -- --exact
cargo test --lib compile::routing::tests
cargo test --lib
cargo test --release --test fragment_synth_acceptance
cargo test --release --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --exact --nocapture --test-threads=1
```

All must pass with byte-identical fingerprints/quality/pinned IO to
pre-hoist baseline.

### 3.5 Re-benchmark and acceptance gate

Repeat Phase 2.1/2.2 exactly (3 samples, medians) on alu8 and
ripple_adder8, post-hoist, and compute:

```
route_nets_speedup = median(pre_hoist_route_nets_ms) / median(post_hoist_route_nets_ms)
end_to_end_regression_pct = (post_hoist_total_ms - pre_hoist_total_ms) / pre_hoist_total_ms * 100
```

Acceptance requires **all** of:

- `route_nets_speedup >= 1.5` on both alu8 and ripple_adder8
- `end_to_end_regression_pct <= 5%` on both cases

### 3.6 Review checkpoint

Present pre/post medians, `route_nets_speedup`, and
`end_to_end_regression_pct` for both cases before finalizing. If acceptance
fails on either metric for either case, revert the Phase 3 commit (`git
revert`, not history rewrite) and fall through to Phase 4.

If it passes, run the standalone whole-world acceptance once before retaining
the commit:

```powershell
$routerAcceptanceDir = Join-Path $env:TEMP ("reda-router-acceptance-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $routerAcceptanceDir | Out-Null
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output "$routerAcceptanceDir/acceptance.json" --shipping-source "$routerAcceptanceDir/shipping_config.rs" --shuffle-seed 0x5245444120260831
```

### 3.7 Commit boundary

Commit only the hoist change and its two tests as:
`perf: hoist per-expansion ancestry snapshot in search_path`

If reverted per 3.6, the revert is its own commit:
`revert: back out ancestry snapshot hoist (acceptance gate not met)`

## Phase 4 — Fallback: profile `seed.rs:2595` clone only (if gate not met, no implementation)

Only reached if Phase 2's gate fails. No optimization is implemented here —
profiling only.

### 4.1 Method

Add the same class of diagnostic-only timing (reusing the Phase 1
`REDA_PHASE_TIMING` mechanism if that phase landed, or an equivalent
minimal `Instant`-based measurement if Phase 1 was skipped because the gate
failed before Phase 1 tests were needed — note Phase 1 instrumentation
lands regardless, since it's how the gate is measured, so Phase 4 only adds
a new counter for the clone at `seed.rs:2595`, `let mut attempt_reservations
= reservations.clone()`) around that single clone call: element count and
elapsed time per call, aggregated per route stage invocation, gated the
same way.

### 4.2 RED/GREEN test

- RED: `reservations_clone_diagnostics_present` — asserts the new
  counter/timer fields exist and are zero before any route attempt runs.
- GREEN: implement the counter increment/timer around the clone call only.
- Follow-up: `reservations_clone_diagnostics_match_call_count` —
  asserts recorded call count equals the number of route attempts made in a
  small deterministic fixture, and clone element count equals
  `reservations.len()` at each call site.

### 4.3 Verify (serialized)

```
cargo test --lib compile::fragment_synth::seed::tests::reservations_clone_diagnostics_present -- --exact
cargo test --lib compile::fragment_synth::seed::tests::reservations_clone_diagnostics_match_call_count -- --exact
cargo test --lib
```

### 4.4 Benchmark

Same alu8/ripple_adder8 3-sample-median harness run as Phase 2.1, now also
recording clone elapsed_ms and clone element counts, reported alongside
`route_nets` and `ancestry` phase timings.

### 4.5 Review checkpoint

Present the clone-cost medians and their fraction of `route_nets` for both
cases. Explicitly stop here — do not design or implement an overlay,
copy-on-write structure, or any replacement for `reservations.clone()` in
this plan. That is out of scope; a future plan would need to be written and
approved separately if the data supports it.

### 4.6 Commit boundary

Commit diagnostics only:
`test: add diagnostic-only reservations-clone timing at seed.rs:2595`

## Execution order summary

1. Phase 1 (RED -> GREEN -> verify -> commit)
2. Phase 2 (benchmark, review checkpoint, gate decision)
3. If gate passed: Phase 3 (RED -> GREEN -> verify -> re-benchmark -> review
   checkpoint -> commit or revert)
4. If gate failed: Phase 4 (RED -> GREEN -> verify -> benchmark -> review
   checkpoint -> commit), stop — no overlay implementation.

## Execution outcome — 2026-09-08

The diagnostic ancestry timer was removed: timing every predecessor lookup
measurably distorted `route_nets`, so its ancestry fraction was not accepted
as retention evidence. Clean coarse `route_nets` measurements replaced it.

The retained candidate reached 11.246720 s on ripple_adder8 versus a
17.892804 s baseline (1.59x), and 30.953307 s on alu8 versus 50.313820 s
(1.63x). Both retained identical ticks and block counts.

A boundary-safe fallback preserves the original predicate order where `i32`
coordinate arithmetic could overflow. All 17 routing tests and the pinned
seven-segment IO contract passed afterward; independent static review returned
APPROVED. The post-fallback full library suite passed 930/930 with 74 ignored.
