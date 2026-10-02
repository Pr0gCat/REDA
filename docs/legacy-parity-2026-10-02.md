# First legacy adapter route divergence

The all-pinned full-adder parity assertion now passes after preserving the historical caller's complete route policy. No route equality assertion, simulator, physical verifier, or production search policy was changed in this experiment.

The original congestion rounds first refuse `g3` in the reference and `g21` in the typed control. These different failure reservations are downstream evidence, not the root cause. The typed legacy callback records claims immediately; transactional staging applies only to strict/seed routing, so missing partial claims is not established as the cause.

An opt-in test-only experiment routes each signal with both controls from the same reservation and congestion state. It compares the complete Route, then reservations, before advancing. Signal `a` agrees; the first divergent Route is `b`. Both `b` terminal kinds remain DirectedDustIntoSupport. The first branch rises to y=3 in both controls, but the reference descends earlier and the typed branch stays at y=3 longer.

The reference deterministic A* prices a vertical step closer to the goal height at 1 and other vertical steps at 3. The typed legacy `search_step_cost(false, false, ...)` prices every vertical step at 3. This difference exists at HEAD and has its own route-mode regression. It precedes the observed congestion failures. TotalStairs refresh reserves alone therefore cannot recover exact historical parity.

No global price change is justified by this experiment: generated routing owns its qualified policy and must preserve its existing independent controls. The next repair must preserve the historical compatibility caller's search pricing explicitly without weakening ordinary generated routing or changing the frozen reference. Afterwards rerun complete Route equality, related routing tests, and physical regressions.

A separate code inspection finds that adapter error conversion ignores the optional sink carried by Refused. This is not the first divergence in this fixture and remains a separate unverified repair candidate.

Diagnostic instrumentation was removed after the experiment. Raw logs, hashes, and the exact comparison instrumentation are retained alongside this note. Both experiment tests fail as expected; this is diagnosis, not a passing qualification or goal completion.

## Qualified repair

The historical compatibility control explicitly selects distance search with its toward-goal height discount and automatic non-merge terminal selection. Its TotalStairs refresh reserve remains unchanged. Normal generated routing retains its existing distance/strength choice, vertical pricing, TerminalStairs reserve, qualified arrival runways, and observed-output isolation. The existing typed kernel is reused; no new router or simulator operation is introduced.

Repairing only search pricing passes two parity tests but still rejects complete full-adder equality at `cin`: the paths agree, but DirectedDustRunway overrides a terminal repeater demanded by the frozen control's strength budget. Restoring the historical automatic terminal contract fixes the remaining discrepancy. All original complete Route equality assertions are retained. The three existing parity tests and all 69 routing tests pass.

This isolated routing change also includes the already qualified strength-search selection wiring: the planner adapter now forwards the actual search model to the existing strength-aware kernel, and the kernel accounts for actual queue/expansion operations using unchanged RouterLimits. The routing regression proves strength pruning differs from blind distance expansion and checks success and zero-budget refusal. Earlier selection regression traces remain in the arithmetic worktree evidence; arithmetic source/growth/clock initialization is not included in this isolated commit.

Both diagnosis logs and the rejected price-only repair are retained. Temporary diagnostic instrumentation is removed. No full root rerun follows this repair, so no updated full-root pass/failure count is claimed. Arithmetic counter qualification, remaining root failures, real-game conformance, and the eight-item goal remain incomplete.
