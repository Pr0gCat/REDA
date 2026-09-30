# Buffered terminal placement space experiment (rejected)

Control: `f20b999`. The prior forced-runway experiment could route `[5,3,2,0]`, but could not obtain a candidate for `[4,1,0,2]` and `[4,3,2,5]`. This iteration keeps its typed buffered-dust requirement and two-cell search runway, and adds one cell to the horizontal placement separation of every body with a routed connection. It is a diagnostic padding approximation for the additional relay approach cell, not a complete per-terminal footprint model. Electrical conductor-clearance constants are unchanged.

All three known fanout defects now produce real candidate route ledgers and pass all 32 setup/target history phases apiece: `[5,3,2,0]`, `[4,1,0,2]`, and `[4,3,2,5]`. Every internal gate driver and output lamp is correct (96 total phases). Each diagnostic requires exact canonical-world equality between the emitted ledger and stock compile before attributing the physical paths. These successes are not legacy-fallback acceptance and do not establish full-family qualification.

This resolves the previous two local-route refusals under the experimental geometry and supports inadequate access space as one cause. It does not establish that a uniform halo is the correct final layout contract. The existing placement code itself distinguishes routing halos from actual channels; no per-terminal ownership is inferred merely from adding separation.

Two independently run quality suites still reject the candidate:

| Circuit | Metric | Experiment | First failing fixed cap |
| --- | --- | ---: | ---: |
| and4 / REDA | Worst output change ticks | 18 | 16 |
| xor2 / native | Driver glitch transitions | 3 | 2 |

The native AND has worst world settling 18 ticks, mean 8.22265625 ticks, 13 output glitch transitions, and zero single-input glitches. The XOR failure occurs at the first checked independent control; all later, potentially stricter controls are preserved as well. No fresh-Yosys success or complete quality-suite success is claimed. A correct final truth table cannot compensate for timing or transient regression.

The full rejected patch is archived as `generator-buffered-layout-rejected-2026-09-30.patch`, based on `f20b999`. The changes to planner, router, and placement separation are withdrawn; all three production files are restored exactly to the control. No test assertion, observation history, input hold, timing/glitch cap, or simulator rule is weakened. The independent endpoint and shipping controls are unchanged.

The physical spacing hypothesis has useful evidence, while universal buffered-NOR replacement remains rejected. The next candidate design must jointly model actual terminal space and avoid adding unjustified stages along critical paths. Whole-circuit source truth, real driver/lamp observations, determinism, and fixed quality controls remain the integration gate; locally balanced inputs alone are insufficient.
