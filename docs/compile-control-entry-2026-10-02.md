# Independent compiler control and selected public result

Two root tests called public `compile()` and assumed it was still the original planner/channel fallback. The qualified rewrite selector now legitimately returns FragmentSynth for `segment_a`; the old assertions fail with FragmentSynth versus Legacy. `compile_without_rewrite()` already owns the original independent pipeline used by selection. No production behavior needs changing.

The six-circuit path test now calls that existing control and retains all six original expected names and PlannerKinds. The fallback test retains the planner-expressibility check, unchanged eight-round trial, actual router refusal assertion and exact Legacy fallback expectation on the control. It additionally calls public `compile()`, requires FragmentSynth and a physical rewrite, and validates the entire public view against its sealed source/world/observation bindings.

Both existing tests pass: the six-circuit matrix in 13.11 seconds and the fallback/public-result check in the retained log. No prior path expectation is repinned, no snapshot is updated, and no simulator, verifier or production selector changes. The before log is an extract of the earlier full-root failure snapshot; the two after logs are fresh targeted runs on the current worktree, which still contains separate unfinished arithmetic/growth/clock work. No updated whole-root count is inferred from these targeted results.

Arithmetic physical qualification, the remaining root failures and all eight goal items remain incomplete.
