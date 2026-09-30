# Mixed NOR input terminals: confirmed backfeed and rejected repairs

This continues the failed row-selection investigation from commit 98b12f8.
The diagnostic now reconstructs the planner route ledger and asserts its exact
emitted world fingerprint equals the actual stock compile() world before
using that ledger to attribute any cell. It prints every terminal's kind and
last path cells, plus the highest-powered cells of the affected route.

## Confirmed mechanism

For the native [1,3,2,0] ROM after from=0,to=3,address=3, every logical gate
output is correct but g2's output lamp at (41,0,42) is incorrectly lit.
All dust around that lamp belongs to g2, not a foreign route crossing beside
the lamp. The highest powered g2 dust is its NOR n1 landing (46,1,38), at
strength 15; power then decreases toward the lamp to strength 4.

n1's support is (45,1,38). Its n0 input is a repeater terminal at (44,1,38)
that strongly powers the support. Its g2 input is bare directed dust on the
opposite face. At address 3, n0 is true while g2's own output torch is off.
A strongly powered conductive support re-drives adjacent dust, so n0 pumps
power into g2's input landing and backward along g2's fanout/output route.
The bug is mixed strong/weak input isolation, not an erroneous Boolean graph,
a torch burnout diagnosis, or a foreign route beside the output lamp.
Existing checks allow declared inputs to reach the same support and do not
refuse this reverse power into another declared input's exposed conductors.

## Experiments and fixed acceptance

1. After all routes were laid, convert every bare input on a support with a
   repeater input into a diode, updating branch repeater metadata. The original
   diagnostic's complete history then has correct internal and lamp values.
   Four constant-driver tests pass, but fresh Yosys AND4 worsens to 14 output
   ticks against its frozen 12-tick cap. This version is rejected.
2. Isolate only routes with public outputs or consumers outside the receiving
   NOR. Native/fresh-Yosys main quality reports pass; constant-driver tests
   pass. But generator_quality_matrix reports majority3/native lamp glitches
   2 against the frozen cap 0. This version is rejected.
3. Check actual reverse dust reach from a landing rather than seeding every
   cell belonging to the whole signal. Existing diodes should stop reverse
   propagation, but the candidate still produces the same majority regression.
4. Attempt to move an existing support repeater upstream to a straight,
   branch-private dust cell within the strength budget, retaining the stage
   count and making the final landing weak dust. Moves for one receiving NOR
   are applied only if every strong input can be transformed. Both the hybrid
   with added isolation and relocation-only versions still fail majority's
   2 > 0 lamp-glitch cap. No full-family correctness or quality qualification
   is claimed for relocation-only.

All generator changes and experimental correctness tests were reverted.
No verifier revision, acceptance cap, independent baseline, hold time or
production selector was changed. The retained changes are route-attribution
and power-source diagnostics plus this record. Production source and acceptance
test files exactly match HEAD after rollback (git diff --exit-code -- src tests).
The post-rollback diagnostic reproduces the original bug with the ledger/world
identity assertion passing. An intermittent rustc SIGBUS during an experiment
was retried; the repeated quality failures above are actual test outcomes,
not compilation failures. git diff --check passes.

The next repair must address both reverse isolation and switching hazards.
Simply adding a stage or assuming an unchanged stage count preserves all
physical timing is disproved by these experiments. The current stock bug
remains open; this is diagnostic progress, not a completed fix or a globally
qualified row-selection flow.

Post-rollback verification: frontend_quality_report and generator_quality_matrix
both pass with every existing cap intact, covering 1312 measured transitions.
The known [1,3,2,0] backfeed is outside those established suites and remains
reproducible by the diagnostic; their passing results do not certify that case.
