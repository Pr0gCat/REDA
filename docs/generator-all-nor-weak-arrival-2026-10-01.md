# Weak NOR endpoints and branch arrival skew

Reference a94d9c8. Generalize the archived required-refresh endpoint rule
from multi-input NOR to every nonconstant combinational NOR, with the existing
observed-output isolation preserved. No fixture names influence placement.
This tests whether a redundant inverter terminal delay contributes to skew.
The additive multi-input approach reservation remains unchanged.

XOR compiled twice deterministically, with a matching Unified3d plan/world
ledger and correct every-gate and independently expected lamp truth throughout
16 setup and 16 target phases. Unbalanced target settling totals 90 ticks
(mean 5.625, worst 10), but driver/lamp glitch transitions remain 2/1.
The unchanged matrix rejects its current lamp glitch cap.

The existing generic diagnostic arrival balancer adds only b->n2 at
(22,1,28). The complete history then totals 84 ticks (mean 5.25, worst 10),
with zero driver/lamp extra transitions or wrong pulses. Two runs are byte
identical. This improves the frozen XOR mean 6.375 locally, unlike the prior
multi-input-only balanced mean 6.75. It is still an uncertified emitted-world
mutation; the final ledger explicitly does not match, and no automatic
compiler balancing or global acceptance is claimed.

The production candidate also fails the unchanged frontend physical suite:
fresh-Yosys AND4 has 9 output glitch transitions against cap zero, world worst
26 and output worst 12. Native AND4 reports world 28, output 16, mean
6.56640625, 13 glitches. These results prevent adopting the weak rule globally.
Both suites were run separately because cargo stops after the first failing
test binary. Complete logs, both XOR histories and rejected patch are saved.
No test cap, simulator, history hold, or block-based acceptance changed.

All three production files are restored byte-for-byte from HEAD. No production
optimization is accepted. Next integrate arrival-aware endpoint choices with
actual certified routes, and test the complete independent physical matrix;
a local delay mutation cannot establish that generic weak terminals qualify.
