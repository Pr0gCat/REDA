# Exhaustive stock ROM probe exposes mixed-support failures and a rejected diode fix

Reference 119a529 (production generator 32df849). Observe actual stock worlds,
including Legacy fallback, rather than attributing every compiled world to an
independently planned Unified3d candidate. The existing route diagnostic refuses
[4,3,2,5] because its newly planned ledger does not equal the Legacy world;
that is an attribution refusal, not proof of incorrect physical logic.
The new manual rom_family_physical_probe tests the actual emitted world.
[4,1,0,2] and [4,3,2,5] both pass 32 retained-history phases.

## Independent complete family control

Enumerate all 4096 four-word, three-bit tables with encoding
word0 | word1<<3 | word2<<6 | word3<<9. Compile native source functions and
repeat compilation to check world fingerprints. Public output aliases share
physical declarations but each public bit is independently expected directly
from the fixed table. All internal gate drivers are checked independently
against source graph evaluation. Named a,b values determine table addresses.
Retain simulator history through every from/to setup and target phase, with
unchanged 2000-tick settling budget; do not reset history or add holds.

The frozen control covers every encoding exactly once: 4068 tables pass all
32 phases; 28 return a concrete incorrect-physics witness. Failed cases stop
at their first wrong gate/output and are not claimed as complete histories.
1507 compiled worlds use Unified3d and 2589 use Legacy. Compile and initial
settling refusal are distinct statuses. Complete JSONL records are preserved,
including actual fingerprints, phase timings, and failure states/netlists.
This is a truth and whole-world-settling probe; it does not measure the nine
output glitch/pulse metrics for all 4096 tables or certify Minecraft behavior.

## Root cause and candidate

Encoding 85, words [5,2,1,0], has g3=false where both the independently
expected third output and the evaluated source gate require true at address 0.
The exact-world route diagnostic finds false n0 wire power 15 and a foreign
active strong landing on their shared receiver. Remove only that attributed
strong landing at (30,1,24), retaining the entire physical history: g3 changes
from false to true after 5 ticks. The causal cut removes a required source and
intentionally fails acceptance; it is never presented as a candidate circuit.

The generic candidate preserves every existing public-output diode and also
requires a diode on other inputs to a receiver containing such a public-output
source, for combinational networks. This prevents strong support power from
returning through a neighbouring dust input into a shared false source net.
It does not inspect words, encodings, module names or case identifiers.
The rejected one-file production patch is retained separately.

Candidate full-family results: 4078 pass, 18 remain incorrect; ten of the
original wrong cases are fixed, and no formerly correct case becomes incorrect.
The fixed frontend/generator (1312 target transitions), inverter quality, DFF
physical, observed-output isolation, and six ROM shipping-cap tests all pass.
The initial rustc build crashed with SIGSEGV; a same-source build retry succeeds
without changing tests or omitting physical checks. Both logs are preserved.

However, comparing every formerly correct table's target settling separately
against the newly frozen independent control finds 71 regressions. [1,2,1,0]
(encoding 81) totals 79->91 ticks and worst 10->12. The previously recovered
[5,3,2,0] (encoding 157) totals 120->126 and worst 12->14. Correctness wins
cannot compensate for slowing other already correct cases. Both complete
4096-table streams and all regression entries are preserved; material/gate
counts never rank acceptance. The candidate is rejected and planner.rs is
restored byte-for-byte to HEAD. Existing fixed caps are not regenerated.

The final manual observer returns a nonzero exit if any case fails, while
still writing every tested encoding. Initial discovery runs predated this
exit-status safeguard; their rows were explicitly inspected and never called
green. Reproduction: source tools/env.sh; RUST_MIN_STACK=33554432;
cargo run --release --example rom_family_physical_probe -- 0 4096.
A range [85,86) reproduces wrong physics; [2716,2717) exercises a correct
Legacy fallback without pretending that it matches a separate plan.

Next seek receiver entry structures that block support-side backfeed without
adding a forward stage, while preserving public observation isolation and
effective route delivery. The newly frozen complete family control adds a
stronger per-table settling comparison to future experiments. The goal remains
active; no universal ROM correctness, timing, or full-repository pass is claimed.

After withdrawal, the unchanged frontend/generator quality suites and expanded
observed-output-isolation regression pass again. The final observer is tested
on encoding 85 (exit 1, full witness exactly equals its frozen control row)
and encoding 2716 (exit 0, all 32 Legacy phases correct). No production source
diff remains. git diff --check passes.
