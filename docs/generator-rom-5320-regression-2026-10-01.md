# Previously incorrect ROM now passes an independent retained-history regression

Current generator 32df849; independent old generator d5074bf. The known
[5,3,2,0] stock ROM previously failed through support-side backfeed. Replaying
the existing manual physical diagnostic on current production now passes all
32 setup/target phases, every internal gate and all output lamps, with its
compiled-world/candidate-ledger fingerprint assertion intact.

Extend the existing observed-output-isolation test with that ROM alongside its
original [1,3,2,0] control. Both run with a,b; renamed left,right; and reversed
input declarations. Every from/to input pair retains simulator history through
setup and target. Public outputs are expected directly from the fixed word
table using actual named input values, independent of generated gate
evaluation and declaration bit order. All internal gate drivers are also
checked against source evaluation, and public-output fanout must retain a
one-way landing. Repeated compilation retains its fingerprint assertion.
The six variants cover 192 physical setup/target phases, with no extra hold,
simulator reset, weakened cap, or physics change.

To establish a real regression control, save all five production files then
replace them exactly with d5074bf source, keeping the identical expanded test.
The old generator physically fails at g3, from=0,to=0,address=0: actual false,
expected true. This is the previously incorrect stable output, not a timing
threshold or reduced block count. Restoring the five production files exactly
to 32df849 makes the same expanded test pass. The final source equality check
is recorded, and complete logs are adjacent. No production source change is
introduced in this commit; it locks in a newly verified effect of the preceding
generic endpoint/layout/arrival optimization.

The current recorded n0->n1 and n0->g3 terminals still include directed dust.
Passing this concrete ROM and its naming/declaration variants therefore does
not prove that every possible mixed-support backfeed path is structurally
impossible. It demonstrates correct complete histories for these functions.
Further work should establish effective one-way paths, including any upstream
refreshes and same-owner bypasses, across broader physical families.

No timing improvement is inferred from this truth regression, and no new
quality caps or controls are regenerated. The preceding generator's fixed
quality results remain documented separately. No complete repository or all
ROM-family claim is made; simulator qualification is not Minecraft proof.
The optimization goal remains active. git diff --check passes.
