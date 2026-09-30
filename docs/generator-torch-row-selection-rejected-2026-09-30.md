# Rejected selection experiment: stock output truth failure

Automatic torch-row selection was attempted by comparing nine boundary metrics
under the same initial settle and 16 from/to transitions, checking setup and
target truth. A row could win only if every metric tied or improved and at
least one strictly improved. Equality retained stock. No size metric ranked
candidates. Neither certified generation nor measurement failure could return
a partially checked winner.

Six frozen shipping comparisons passed: the first two complex functions chose
rows, two regressions and two constant ties retained stock. Repeated selection
was deterministic. Non-aliased measurements matched the prior independent
shipping JSON exactly. The aliased [0,0,0,7] case exposed a measurement issue:
the old probe counts events by position, while its observer registers each
alias separately, so duplicate registrations falsely count one physical change
as a glitch. With one physical event stream per coordinate and attribution to
each public alias, driver/lamp glitch totals are 3/3 rather than the old 21/21.
Pulse totals and settling remain unchanged. This was not a lost-label problem:
the observer does preserve multiple labels at the same coordinate. Prior frozen
controls were retained untouched, not overwritten with revised counts.

Full-family selection qualification then failed at encoding 153, table
[1,3,2,0]. Stock compile() returns Unified3d. Under the retained history,
from=0,to=3,address=3, after 14 ticks every internal gate observation is
correct, but output g2's lamp at (41,0,42) stays lit although g2 is false.
The g2 wall torch at (41,1,43), facing north, is off. The dust above the lamp
at (41,1,42) has power 4; adjacent dust at (42,1,42) has power 5 and at
(42,1,41) has power 6. This locates the defect in physical output delivery,
not the source Boolean function or the matrix proof. The exact foreign route
and the reason static physical checks admitted it remain to be traced.

The complete selection experiment did not pass. Its module, export, shared
truth-table refactor and experimental tests were reverted. Stock compile(),
the qualified torch-row entry and all existing acceptance tests remain exactly
as commit add79d2. No invalid truth was accepted, no extra hold time was added,
no failed full-family test was reclassified as passing, and no automatic
selection was enabled. The next optimization cycle must diagnose this output
backfeed and implement a general routing/isolation fix before qualifying the
complete stock/row choice.

The retained manual diagnostic is examples/rom_output_history_diagnostic.rs.
Run `source tools/env.sh` then
`RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic`.
It reconstructs the real native frontend source, checks every physical internal
gate against the Boolean graph for each history phase, and reports the first
wrong declared lamp plus neighboring states. The final diagnostic reproduced
the coordinates/history above. Its successful diagnostic exit is not an
acceptance claim. Production source and test files have no diff after rollback;
git diff --check passes. This records a failed experiment, not a quality fix
or completion of the ongoing goal.
