# Task 3 report

## Changed files
- `src/compile/fragment_synth/hierarchy_api.rs`, `#[cfg(test)] mod tests` only:
  - Added `use crate::compile::fragment_synth::search::StopReason;` to the
    test imports.
  - Added the test-only fixture `chain_of_five_not_blocks`: a top with no
    gates of its own chaining five one-gate NOT blocks (alternating between
    two distinct leaf modules, `not_a`/`not_b`), so the design wires up four
    block-to-block edges (`g0->g1`, `g1->g2`, `g2->g3`, `g3->g4`); `a->g0` and
    `g4->z` are primary-input/output bindings, not block edges. This is
    stated on the fixture's doc comment, not asserted by any test code.
  - Added the characterization test
    `evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases`, which
    runs `compile_hierarchical_with_threads` over that fixture at evaluation
    budgets 0/1/2/4 and checks: each budget's `evaluations_used` and
    `trace.len()` equal the budget; every result's `stop_reason` is
    `StopReason::EvaluationBudget`; each smaller budget's trace is an exact
    prefix of budget 4's trace; quality is a non-increasing staircase as the
    budget grows and never worse than budget 0; and a 4-worker run at budget
    4 agrees with the 1-worker run on fingerprint, quality, and trace.
  - Wording-only edit to that same test's own assertion message: the local
    variable is named `budget_4_trace`, and the message on
    `budget_4_trace.len() == 4` now states only that budget 4 evaluates
    exactly four proposals before `EvaluationBudget` stops it, pointing at
    the doc comment on `chain_of_five_not_blocks` for the fixture's edge
    count rather than claiming the length itself proves it.
  - No production code was touched.

## Characterization result
No artificial RED was produced. The characterization test named in Task 3 was
run against the existing code as-is and passed on its first run — the
behaviour it characterizes (deterministic quality staircases across
evaluation budgets 0/1/2/4) was already correct in `hierarchy_api.rs` before
this task started.

## GREEN evidence (from controller)
- `cargo test --lib evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases -- --nocapture`: PASS, 1 passed / 958 filtered, test time 9.62s.
- `cargo test --lib a_time_budget_stops_only_after_the_crossing_proposal_finishes -- --nocapture`: PASS, 1 passed / 958 filtered, test time 0.00s.
- Only warning present: the pre-existing `BlockFacts.delay_ticks` dead_code warning, unrelated to this change.

## No-production-code conclusion
Because the characterization test passed immediately, no production code
change was required or made. Task 3 therefore consists only of the new
test-only fixture, the budget-contract characterization test, its test import,
and the wording correction described above.

## Self-review
- `budget_4_trace.len() == 4` proves exactly four evaluations ran before the
  search stopped at `EvaluationBudget` — it does not prove the fixture cannot
  offer a fifth edge. That the source wiring in `chain_of_five_not_blocks`
  logically creates exactly four block edges is documented on that fixture's
  doc comment, not asserted by any runnable check. The trace-length assertion
  and the doc comment are two different kinds of evidence; the message was
  corrected so it does not call the doc comment an assertion or claim the
  trace length alone proves the fixture's edge count.
- Budget 5 was deliberately not added: Task 3 only requires evaluation
  budgets 0/1/2/4.
- The worker-count comparison (`many` vs `results[3]`) at budget 4 was
  retained rather than moved to a smaller budget, since it is the strongest
  version of that check and the total test runtime is 9.62s, which is cheap
  enough that there is no pressure to trim it.
- No other assertions in this test or in
  `a_time_budget_stops_only_after_the_crossing_proposal_finishes` make the
  same overclaim, so no further wording changes were needed.
