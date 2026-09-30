# Constant drivers and complete torch-row ROM qualification

The stock compiler now emits real constant outputs: NOR(0) is a wall torch on
an unpowered conductive support, and NOR(1) inverts it for zero. The native
frontend supports literal/folded outputs and constant DFF data. Fresh Yosys
literal output bits use these drivers too. Yosys internal constant gate pins
retain their existing folding/rejection rules; x/z remain unsupported.

This also closes the 1352 previously excluded constant-output ROM tables.
The prototype's complete two-address, three-output family now has 4096 linked
native-source rewrite and physical proofs, full ExpandedStrict certification,
exact emission, deterministic reconstruction and internal truth checks.
Production matrix selection and original-source rewrite attribution remain
open; public compile still uses its existing generator for ROM logic.

## Observation, cause and general fix

The initial all-zero matrix produced correct final values but settled in 38
ticks against the independent control's 16. Unused address inverters could
burn out under the actual rapid transition history. Remove inverse gates
without consumers, their physical torches, and rails beyond the last real tap.
No table-name special cases, longer input holds or relaxed caps are used.
Constant columns receive explicit high drivers and low inverters; constant-one
rows require no address decoder input. Torch supports remain protected by the
physical coupling verifier. A two-cell foreign dust run directed into a
constant support is rejected; an isolated dot was an ineffective negative
fixture and was replaced with the explicit directed run.

## Fixed controls and measured result

The aligned-refresh prototype control is unchanged. The exact typed adapter
fixture from commit 39257ff was separately replayed before this fix to freeze
all nine metrics for each of the 2744 nonconstant tables. Its total is 420816,
matching the committed prior result. These independent per-table caps are
stored in generator-torch-row-prior-nonconstant-caps-2026-09-30.json and checked
in addition to the original prototype control. They must not be regenerated
from the current candidate.

Each arm measures 65536 target transitions, with setup transitions and a
separate internal-node truth replay. All nine metrics pass individually for
every table, and every previous nonconstant cap is preserved. Total settling
is 611056 control versus 608752 candidate: 2304 ticks saved over 120 tables.
Worst settling remains 16 ticks. Block/gate counts are not acceptance metrics.
The adjacent JSON records the proof fingerprint and aggregate results.

These are simulator results, not Minecraft certification or a guaranteed
clock rate. Existing production quality limits remain unchanged.

## Validation

Full-family physical/proof/timing regression and the foreign-support negative
test pass. An initial release build crashed inside rustc; identical retry with
RUST_MIN_STACK=16777216 succeeded. No test or quality limit was weakened.
Focused topology/library tests: 33; typed topology: 9; Yosys JSON: 8;
fragment API: 14. Constant-driver physical tests: 4. Native compiler/DFF/fuzz
regression: 37 tests, including the unchanged 10000-expression corpus.
Production quality suites preserve all existing caps over 1312 measured
transitions. Matrix restoration negative checks, foreign-support rejection,
and the fresh-Yosys/wrong-source proof chain pass. git diff --check passes.
Existing unrelated warnings remain; this is not a claim that the whole
repository suite is green.
