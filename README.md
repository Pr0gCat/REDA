# REDA attic

Uncommitted work rescued from worktrees deleted on 2026-08-25. Nothing here
is applicable as-is; it is kept because the *ideas* may still be worth
something, not the patches.

## 2026-08-10-task3-fix-planner-diagnostics.patch

From the worktree `REDA-task3-fix`, branch `codex/global-polarity-task3fix`,
base commit `0e99d90`. 189 lines across `src/compile/mod.rs`,
`src/compile/planner.rs`, `tests/verilog_frontend.rs`.

**What it was:** an early version of the hybrid `compile()` — try the planner,
fall back to the legacy seed on failure. The same structure that eventually
shipped as `e155243`, reached independently and earlier.

**Why it will not apply:** the APIs it is written against are gone.
`realise_candidate`, `PlannerError::CandidateNotRealisable`,
`PlannerDiagnostics`, `PlannerKind::LegacySeedAdapter` and
`compiled_from_legacy_emission` do not exist in `main`, and `verify_candidate`
has been rewritten since.

**The one idea worth recovering:** `PlannerDiagnostics` recorded
`evaluated_candidates`, `rejected_unrealisable_candidates` and
`selected_legacy_seed`. The shipped `PlannerKind` records only *which path a
circuit took*, not *how much was tried and refused on the way*. If the trial
phase ever needs to explain itself — and the hybrid's trial cost has been a
recurring question — this is the shape that answer took the first time.

Not kept: `global-polarity.patch`, two `topology.rs` tests
(`every_negative_expansion_computes_the_complement_of_its_gate`,
`nand_is_the_negative_realisation_of_and`). Both are already in `main`
verbatim, so the draft had nothing the tree lacks.

## gp-clean.patch

From `.worktrees/global-polarity-clean`, branch `codex/global-polarity-clean`,
base `55b6bab`. 144 lines, all of it new tests in `src/compile/polarity.rs`.

**Five tests, none of which exist in `main` today** (checked by name):

- `assignment_does_not_panic_for_unregistered_direct_gate_arities` — `Nor(4)`
  and `Or(1)` look realisable to lowering but have no library entry; a public
  fallible API must report, not panic.
- `score_counts_shared_inverters_once_across_reconvergent_fanout`
- `pair_flip_escapes_a_single_flip_local_minimum` — a strict single-flip local
  minimum that only a pair sweep escapes.
- `directly_realisable_nor_and_or_gates_are_not_polarity_eligible`
- `score_orders_area_then_gates_then_torch_depth` — **BROKEN, do not lift as
  written.** It asserts `lower_area < fewer_gates` and `fewer_gates <
  lower_area` on consecutive lines. Both cannot hold; this is almost certainly
  why the file was never committed. The ordering it means to pin (area, then
  gate count, then torch depth) is worth a test — just not this one.

## gp-assignment.patch

From `.worktrees/global-polarity-assignment`, branch
`codex/global-polarity-assignment`, base `3ce2cf0`. 1,370 lines across
`src/compile/lowering.rs` and `src/compile/topology.rs`. **NOT READ.** Deferred
deliberately on 2026-08-25 rather than reviewed and summarised, so this entry
makes no claim about what is in it. Both files are core to the polarity
assignment work and have been rewritten since, so expect it not to apply.

## project.rs.2026-08-13-snapshot

Was `project_real_backup.rs`, loose in the repository root. A mid-Task-7
working copy of `src/compile/relax/project.rs`, matching no committed
revision. Kept only for completeness: every symbol it defines exists in the
current `project.rs`, so it holds nothing the tree lacks.
