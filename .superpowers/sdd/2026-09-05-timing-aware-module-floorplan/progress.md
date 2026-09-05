# SDD ledger — plan: docs/superpowers/plans/2026-09-05-timing-aware-module-floorplan.md

## Preflight interface scan

| Producer | Consumer | Shared interface | Result |
|---|---|---|---|
| Task 1 | Task 2 | `plan_parent_with_services` and block placement offsets | Consistent: Task 1 lands the separate map before Task 2 constructs proposals. |
| Task 2 | Task 3 | hierarchical candidate wrapper, proposal order and trace | Consistent: Task 3 tests the exact public budget behaviour Task 2 exposes. |
| Task 2 | Task 4 | finite proposal stream and `StopReason` | Consistent: exhaustion uses an evaluation cap larger than the finite edge count. |
| Task 1 | Task 4 | `seed.rs` production path and ignored target harness | Consistent: harness consumes production behaviour; it does not duplicate it. |
| Tasks 1-4 | Task 5 | code and verification commands | Consistent: Task 5 changes no interface. |

| Task | Internal consistency | Result |
|---|---|---|
| Task 1 | Test names moved block/body/ports; implementation applies offset before occupancy and geometry registration. | Clean. |
| Task 2 | Tests name stable identity/order/acceptance; implementation uses typed parent assignments and whole-incumbent state. | Clean after spec-review fixes. |
| Task 3 | Prefix and monotonic assertions match existing `run_budgeted_proposals` semantics. | Clean. |
| Task 4 | Success requires both numeric gates; failure requires real stream exhaustion. | Clean after spec-review fixes. |
| Task 5 | Verification scope covers changed hierarchical path and unchanged flat/pinned paths. | Clean. |

Ruling: hierarchical budgets try block-edge proposals before retaining any parent-gate fragment search — the active objective is module-connection latency and the old flat-identity stream wastes work on block internals and duplicates — cost if wrong: a hierarchical design whose useful budget move is only a top-level glue gate may lose that optional improvement until streams are composed.

Evidence before implementation: programmatic same-netlist flat 620 ticks / 132561 blocks / static 672; hierarchical 608 / 70603 / 678. Verilog same-netlist flat 358 / 69757 / 442; hierarchical 366 / 48467 / 522. Programmatic block carry-port lateral mismatch is 36-52 cells across all seven edges.

Spec review: APPROVED after closing typed `BlockEdge`, whole-incumbent wrapper and target-oracle blockers.

## Task 1 complete — block placement override

- Commits: `27b6c2b`, `2e63673`.
- RED: `block_placement_override_moves_only_the_selected_block_body_and_ports` compiled, then failed because the selected block stayed at its baseline coordinates.
- GREEN: the focused override test passed (0.43s); the existing block-boundary repeater test passed (18.38s).
- Review: initial Important finding that a 3D `Offset` silently ignored `dy`; a direct vertical-relocation experiment correctly exposed `NoLocalRoute`, so the input contract was narrowed to `BlockPlacementOffset { dx, dz }`. Scoped re-review: all findings addressed, no new Critical/Important breakage.
- Result: block body, input targets and output sources now consume one separate lateral block map before occupancy and routing; flat and existing hierarchical callers pass an empty map.

## Task 2 complete — deterministic block-edge proposals

- Commits: `f7008d7`, `e15f9db`.
- RED: pure tests failed only because `BlockEdge`, `explicit_block_edges`, and `block_alignment_proposal` did not exist.
- GREEN: edge extraction/order, cumulative alignment, parent proposal guards and all non-ignored hierarchy API tests are covered; the strict ripple2 integration passed in 205.27s and proved a carry-alignment proposal completed plan, union and certification.
- Review: Claude found that the first integration rewrite allowed a 100%-refusing stream to pass. The test now requires `certified_quality.is_some()`. Re-review: addressed, no new Critical/Important breakage.
- Result: hierarchical tops use a finite, stable block-edge stream; whole accepted candidate state owns relative and realised offsets; flat tops retain the original flat path. Fingerprints use versioned serialized descriptors and compile errors retain the existing terminal/cap-work mapping.

## Task 3 complete — deterministic budget staircase

- Commit: `fc4b7fb`.
- Characterization: evaluation budgets 0/1/2/4 produce exact trace prefixes, non-increasing `QualityKey`, and no result worse than budget 0; worker counts 1 and 4 agree on fingerprint, quality, and trace.
- Time budget: reused `a_time_budget_stops_only_after_the_crossing_proposal_finishes`; no second clock or scheduler was added.
- GREEN: the hierarchy staircase test passed in 9.54s; the existing time-boundary test passed in 0.00s.
- Review: Claude Code Sonnet approved both spec compliance and task quality with no Critical, Important, or Minor findings.
- Result: Task 3 added only a focused fixture and runnable contract test; production behavior was already correct and remained unchanged.
