# Candidate-ledger arrival balancing rejected by AND settling

Reference 0ca543b. Apply the archived all-NOR weak endpoint experiment, then
retime the candidate after routing, before normal candidate realization and
verification. A transactional clone computes topological nominal arrivals from
actual route states, inserts delay-1 repeaters only on private straight dust
cells with recorded solid floors, and updates every affected branch repeater
count. Missing geometry refuses the complete retiming clone. No case names,
block counts, acceptance caps, simulator behavior or extra holds are used.
The original route/world diagnostic now matches the final emitted world: this
is no longer a post-emission mutation. Matching metadata still does not prove
that each nominal path dominates all possible electrical deliveries.

Native XOR passes all internal gate and independent lamp truth checks in
16 setup plus 16 target phases with retained history. The compiled candidate
is deterministic; two complete diagnostic runs have identical stdout. Target
settling totals 84 (mean 5.25, worst 10), versus frozen mean 6.375. Driver/lamp
glitches and wrong pulses are zero. The unchanged generator quality matrix
passes its complete circuit/frontend cases and individual caps.

The separately executed frontend quality suite rejects native AND4 total
settling 2271 against unchanged cap 1681. It stops at that failure, so no fresh
Yosys or seven-segment coverage from this run is claimed. Full terminal histories
and both test logs are saved in the adjacent JSON. The successful XOR and
matrix cannot compensate for the frontend regression. The candidate is not
qualified for production; all three production files were restored exactly
from HEAD. The rejected patch is retained for review and reproduction.

Next reduce original branch delay or constrain retiming by reconvergence
and physical pulse evidence, rather than charging every multi-input gate
additional synchronization. Such a rule must be connectivity-based and
validated across independent fixed controls, not a named XOR exception.
Simulator qualification is not Minecraft certification. Goal remains active.
