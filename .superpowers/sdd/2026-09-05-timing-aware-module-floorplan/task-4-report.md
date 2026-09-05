# Task 4 report

## Changed files
- `src/compile/fragment_synth/seed.rs`, inside `mod tests > pub(crate) mod
  extra_circuits` only (the module that already owns the release-only
  hierarchical acceptance harness `every_hierarchical_circuit_certifies_through_module_floorplan`):
  added one new `#[ignore]` test, `ripple_adder8_hierarchical_budget_target_oracle`,
  directly after it. No production code was touched; no other file in `src/`
  was edited.

## What it does
Runs the real, programmatic hierarchical `ripple_adder8`
(`crate::circuits::hierarchical_builder::circuits::ripple_adder(8)`, the same
fixture `every_hierarchical_circuit_certifies_through_module_floorplan`
already certifies) through `compile_hierarchical` at five points:

1. `SynthesisBudget::Evaluations(0)`
2. `SynthesisBudget::Evaluations(1)`
3. `SynthesisBudget::Evaluations(2)`
4. `SynthesisBudget::Evaluations(4)`
5. `SynthesisBudget::Evaluations(u64::MAX)` — no arbitrary cap, so this point
   can only end when the finite block-edge proposal stream itself runs out

For each point it prints one `TARGET <label>: ...` line to stderr with
`ticks` (`quality.observed_settle`), `blocks` (`quality.non_air_blocks`),
`static_delay` (`quality.static_routed_delay`, `Debug`-formatted
`ExactDelay`), `evaluations_used`, `stop_reason`, `terminals` (every
`ProposalTrace::terminal` in `result.trace`, in order), `fingerprint`
(`result.candidate_fingerprint.as_str()`), and wall time.

Verdict logic, applied after all five points have run:
- **Success**: if any single point's certified result has BOTH
  `observed_settle <= 474` AND `non_air_blocks <= 100_615` at once (checked
  off the same `QualityKey`, so the two gates cannot be satisfied by two
  different runs) — the test then passes without any further assertion.
- **Bounded negative result**: regardless of whether any point meets both
  gates, the test unconditionally asserts that the *last* point
  (`SynthesisBudget::Evaluations(u64::MAX)`) has `stop_reason ==
  StopReason::ProposalStreamExhausted` (plus `evaluations_used < u64::MAX`
  as a sanity bound on that evidence). If that point instead reported
  `EvaluationBudget`, the assertion fails loudly — an uncapped run stopping
  on `EvaluationBudget` would be a contradiction, not just an unbounded
  negative result.

## Why an uncapped budget is guaranteed to exhaust the stream (read, not run)
`HierarchicalProposalStream::next` (`hierarchy_api.rs:570-596`) indexes
directly into `self.edges` with `proposal_index` and returns `None` once the
index runs past the end — one proposal per edge, never more, never
regenerated. `self.edges` comes from `explicit_block_edges`
(`hierarchy_api.rs:388-451`), which is called once per `compile_hierarchical`
run over the *parent's own* block-to-block assignments — for `ripple_adder8`
(8 chained `full_adder` blocks) that is a finite, fixed number of
inter-block edges. With `SynthesisBudget::Evaluations(u64::MAX)` there is no
evaluation cap to hit first, so `run_budgeted_proposals` (`search.rs:207-257`)
must eventually hit `proposals.next(..) == None` —
`StopReason::ProposalStreamExhausted` — rather than `StopReason::EvaluationBudget`.
This is read from the source, not measured; the controller's actual run is
what confirms the real `evaluations_used` and `stop_reason` at this point —
see "Not executed" below.

## Reused, not duplicated
- Fixture: `crate::circuits::hierarchical_builder::circuits::ripple_adder(8)`
  (`h::ripple_adder(8)`), the same import alias
  `every_hierarchical_circuit_certifies_through_module_floorplan` uses one
  function above.
- Compile entry point: `crate::compile::compile_hierarchical` (Task 1-2's
  production front door), called directly with `None` pins, exactly as
  `run_hierarchical_cases` and the Task 3 staircase test do.
- Budget/result types: `SynthesisBudget` (already imported at the top of
  `extra_circuits`), `StopReason` (imported locally, same import path Task
  3's `hierarchy_api.rs` test uses), `SynthesisResult`'s existing fields
  (`metrics`, `trace`, `evaluations_used`, `stop_reason`,
  `candidate_fingerprint`) and `QualityKey`'s existing fields
  (`observed_settle`, `non_air_blocks`, `static_routed_delay`). No new type,
  wrapper, CLI, scheduler, or clock was added.

## Exact commands (controller executes; not run by me)
```
cargo test --release --lib ripple_adder8_hierarchical_budget_target_oracle -- --ignored --nocapture
```
This filters to the single new test by substring match on its full path
(`compile::fragment_synth::seed::tests::extra_circuits::ripple_adder8_hierarchical_budget_target_oracle`),
matching the filter style already used by every sibling `#[ignore]` message in
this file (e.g. `every_hierarchical_circuit`, `large_circuits`).

## Expected output
- Five `TARGET budget=N: ticks=.. blocks=.. static_delay=.. evaluations_used=.. stop_reason=.. case_fingerprint=.. candidate_fingerprint=.. trace=[..] in ..` lines on stderr, one per budget point, in the order 0, 1, 2, 4, exhaustion (`SynthesisBudget::Evaluations(u64::MAX)`).
- `budget=0..4`'s `evaluations_used` equal to the budget number and
  `stop_reason == EvaluationBudget` (matching the Task 3 staircase contract,
  since this is the same kind of run against a different, real fixture).
- `budget=exhaustion`'s `stop_reason == ProposalStreamExhausted` with
  `evaluations_used < u64::MAX` — the exact `evaluations_used` value (i.e.
  the real block-edge count) is not asserted or predicted here and is left
  for the controller's actual run to report.
- Overall test result: PASS either way, because the test's own final
  assertion encodes the brief's two-way success/bounded-negative-result
  contract — a plain PASS does not by itself say which branch fired; that has
  to be read off the printed `TARGET` lines, specifically whether any one
  line has `ticks<=474` and `blocks<=100615` together.

## Self-review
- **Same-result gate check**: `target_met` is computed from
  `quality.observed_settle` and `quality.non_air_blocks` read off the same
  local `quality: QualityKey` value inside one loop iteration, so the two
  gates can never be satisfied by mixing two different certified results.
- **Negative-result bound is on the real last point**: `last` is overwritten
  every iteration and only read after the loop, so today — with the
  exhaustion point placed last in `points` — it correctly names the
  uncapped-budget run's `stop_reason`. This depends on the exhaustion point
  actually being last in the array; it is not a claim that reordering
  `points` would still be safe.
- **No production behavior changed**: only additions inside `#[cfg(test)]`
  test code (`mod tests`); grep of the diff confirms no line outside that
  module changed.
- **No new abstraction**: the test is one straight-line function; the only
  new items are two local `const`s and a fixed-size array literal, no helper
  function, trait, or struct.
- **Risk — exhaustion argument is read from source, not measured**: the
  "an uncapped budget can only stop by stream exhaustion" argument is
  derived by reading `explicit_block_edges` and
  `HierarchicalProposalStream::next`'s source (see previous section), not by
  running the release harness — the controller's actual run is the first
  real confirmation of the real `evaluations_used` and `stop_reason` at that
  point.
- **Wall time**: not bounded or asserted; `every_hierarchical_circuit_certifies_through_module_floorplan`
  already documents `ripple_adder8` as one of the faster entries in that
  suite, and this harness runs the same design five times (budgets 0, 1, 2,
  4, and uncapped each recompile the whole design from scratch, mirroring
  how `evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases` also
  reruns its fixture once per budget) — expect five sequential
  `compile_hierarchical` release compiles, not one.

## Not executed (controller's job)
- The release harness itself: I did not run `cargo test --release`. All
  claims above about `stop_reason`, `evaluations_used`, and the actual
  `ticks`/`blocks`/`static_delay`/fingerprint numbers at each budget point are
  derived from reading `search.rs`, `hierarchy_api.rs`, and `certification.rs`
  source, not from a live run.
- Whether any of the five points actually meets `observed_settle <= 474 &&
  non_air_blocks <= 100_615` — that is exactly the open question this task
  exists to answer, and it can only be answered by the controller's real run.
- Whether the repeater-sharing design doc needs to be started next depends
  entirely on that run's outcome, per the brief's instruction to proceed to
  it only if no point passes and the bounded negative result holds.

## Fix round 1

Three findings from review, all fixed inside the same test (still only
`seed.rs` test code, no production change):

1. **Exhaustion assertion was conditional, now unconditional.** The brief
   says the run "at true stream exhaustion" must be recorded regardless of
   outcome; the original code only asserted the last point's `stop_reason`
   when `target_met` was `false`, so a smaller budget meeting both quality
   gates would have silently skipped checking whether the largest budget
   point actually exhausted the stream or merely hit its evaluation cap. The
   `StopReason::ProposalStreamExhausted` assertion (and a new companion
   assertion, `evaluations_used < u64::MAX`) now run every time, after the
   loop, unconditionally.
2. **Trace/fingerprint logging was incomplete, now complete.** The old print
   line had only `terminals` (just the `ProposalTerminal` enum per entry) and
   `result.candidate_fingerprint`. It now also prints `result.case_fingerprint`
   and the full `result.trace` (`Debug`-formatted `Vec<ProposalTrace>`), which
   carries every entry's `parent_fingerprint`, `fragment_fingerprint`, and
   `choice_fingerprint` already, so no new summary type was needed.
3. **Arbitrary `64` replaced with `SynthesisBudget::Evaluations(u64::MAX)`.**
   The exhaustion point no longer depends on `64` staying larger than
   `ripple_adder8`'s current edge count; an uncapped evaluations budget can
   only end by the proposal stream itself running out, so the assertion is
   evidence read off the real `stop_reason`, not an assumption about the
   proposal stream's current size. The doc comment above the test was
   updated to match (drops the "seven edges" framing in favor of "uncapped
   run can only stop by exhaustion").

### Self-review (fix round 1)
- The unconditional assertion still names the real last-iteration value
  (`last`, overwritten every loop pass), not a hardcoded index or budget
  point, so it stays correct if the point list is ever reordered.
- `evaluations_used < u64::MAX` is a sanity bound on the exhaustion evidence
  itself (a stream that ran to `u64::MAX` evaluations without stopping would
  not be "exhausted", it would just be a coincidentally-equal cap) — cheap
  and always true for `ripple_adder8`'s finite edge count, so it does not
  change what the test proves, only guards against a degenerate future
  where `evaluations_used` and `u64::MAX` happen to collide.
- No new type was introduced for trace/fingerprint reporting; `ProposalTrace`
  already derives `Debug`, so `{:?}` on the whole `Vec` is the minimal way to
  surface parent/fragment/choice fingerprints per the finding, matching the
  "least code that works" instruction from the round-1 task.
- Still only test code changed: the diff touches lines inside
  `ripple_adder8_hierarchical_budget_target_oracle` and its doc comment only.

## Fix round 2

Documentation and formatting only, no logic change:

1. **Stale narrative removed from the sections above.** "What it does", "Why
   budget 64...", "Expected output", and "Self-review" still described the
   pre-round-1 test (five points ending at a hardcoded
   `SynthesisBudget::Evaluations(64)`, `evaluations_used == 7`, "budget 64 is
   larger than the 7-edge count"). Those sections are rewritten to match the
   actual round-1 code: the fifth point is
   `SynthesisBudget::Evaluations(u64::MAX)`, and the real
   `evaluations_used`/`stop_reason` at that point are left to the
   controller's actual run, not predicted as a specific number.
2. **Removed the "if the point list is ever reordered, `last` still means
   the largest budget" claim.** That was wrong: `last` is just whatever ran
   last in the loop, so it only names the uncapped-budget run because the
   exhaustion point is currently placed last in the `points` array, not
   because the assertion is reorder-safe in general. The self-review bullet
   now says exactly that.
3. **`seed.rs` formatting.** The `("budget=exhaustion",
   SynthesisBudget::Evaluations(u64::MAX))` tuple was reflowed across three
   lines by a prior edit; collapsed back to one line (fits under the normal
   width), matching how the other four point tuples are written. No
   behavior change.

### Self-review (fix round 2)
- Reread the full report top to bottom after editing; no remaining mention
  of a hardcoded `64` budget, `evaluations_used == 7`, or "seven edges"
  outside the Fix round 1 section, which correctly describes that as a past
  state that was replaced.
- Only `task-4-report.md` and the one tuple's line-wrapping in `seed.rs`
  were touched; no test logic, assertion, or doc-comment content in `seed.rs`
  changed.

## Controller release measurement

Command:

```text
cargo test --release --lib ripple_adder8_hierarchical_budget_target_oracle -- --ignored --nocapture
```

Result: PASS; 1 passed, 959 filtered out, total test time 2010.81s. The
pre-existing `BlockFacts.delay_ticks` dead-code warning was the only warning.

All runs had case fingerprint
`b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`.

| Budget | Ticks | Blocks | Static | Evaluations | Stop | Candidate fingerprint | Run time |
|---:|---:|---:|---:|---:|---|---|---:|
| 0 | 608 | 70,603 | 678 | 0 | `EvaluationBudget` | `a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2` | 107.224s |
| 1 | 590 | 70,659 | 678 | 1 | `EvaluationBudget` | `7fa6eef709ab48e744e019619ee32828a546d9cab7d196ba917baf6bc9f5afaf` | 208.591s |
| 2 | 590 | 70,659 | 678 | 2 | `EvaluationBudget` | `7fa6eef709ab48e744e019619ee32828a546d9cab7d196ba917baf6bc9f5afaf` | 316.893s |
| 4 | 580 | 70,827 | 678 | 4 | `EvaluationBudget` | `73d0a98984b9a015eb389fa5a19a5e552a957972e4065c04d4a7bb78fe1b9269` | 529.113s |
| exhausted | 576 | 71,147 | 676 | 7 | `ProposalStreamExhausted` | `44f7e78033a4e2a22fea5d1d489f0f4bf0d4be2f446d2ee8e1020a42804065c9` | 848.905s |

Exhausted-stream proposal terminals and certified qualities, in stable order:

| Index | Terminal | Certified ticks | Certified blocks | Static | Accepted |
|---:|---|---:|---:|---:|---|
| 0 | `Accepted` | 590 | 70,659 | 678 | yes |
| 1 | `NoImprovement` | 606 | 70,891 | 676 | no |
| 2 | `Accepted` | 580 | 70,827 | 678 | yes |
| 3 | `NoImprovement` | 580 | 70,899 | 676 | no |
| 4 | `NoImprovement` | 580 | 71,115 | 676 | no |
| 5 | `NoImprovement` | 580 | 71,315 | 674 | no |
| 6 | `Accepted` | 576 | 71,147 | 676 | yes |

## Bounded conclusion

The finite seven-proposal block-alignment stream exhausted normally. Its best
fully certified candidate improves the hierarchical baseline from 608 to 576
ticks and remains 29,468 blocks below the 100,615 block cap, but misses the
474-tick latency target by 102 ticks. Therefore the two-gate target is not
reachable by this complete alignment stream under the unchanged module-boundary
repeater contract. Per the specification, the next smallest design is
single-consumer block-output-to-block-input repeater sharing, retaining the
source output repeater and falling back unless geometry, direction, ownership,
and signal-strength constraints all hold.
