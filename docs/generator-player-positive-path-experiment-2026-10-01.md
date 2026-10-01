# Positive literal paths in the player ROM generator

Baseline: committed `ef8fed8`, using the candidate arm of the frozen buffer-elision family. The shipping control arm remains byte-for-byte checked. Automatic `compile()` selection remains disabled. No simulator semantics, timing holds, history resets, or frozen caps are changed.

## Problem and general solution

A cover cube containing a single positive input literal still used an input NOR torch followed by a data NOR torch. For example, output `y=a` required two inversions and retained torch switching history even though its Boolean function is noninverting. The generator now recognizes a one-bit cube mask whose selected bit is positive, and emits a noninverting control repeater and a noninverting data repeater. All output occurrences driven by that shared cube use this construction. The rule depends on the cover structure, not table IDs or benchmark names.

The data repeater reads a solid support weakly powered by the word rail and drives a solid support with dust below it, next to the shared output bitline. Four-block row spacing keeps repeater floors and landing dust away from adjacent rows and the lamp's outbound tap. Wider layouts require two same-stage input repeaters; a shorter elevated north-side lane feeds the second repeater without adding a timing stage. Torch controls and reconverging outputs containing a data torch retain their input buffers. Pure positive-literal merges may omit these buffers.

## Experiment and failure

The first complete enumeration stopped after 2,080 records: encoding 2080 requested a positive literal on output 2, whose side input is at z=17, while the word rail ended at z=16. The conductor-path assertion rejected the missing connection; the same defect also failed the wide-cover regression. The aborted raw records and log are preserved. The correction extends the word rail to z=17 when a positive literal drives the final output. No verifier relaxation was used.

## Fixed-control results

After correction, all 4,096 tables pass source rewrite and instance equivalence, strict physical certification, and all 32 retained-history setup/target phases per table, including observations of every implemented logic node. Each world is built twice and its fingerprint checked.

Compared independently on all twelve target waveform metrics against the frozen certified baseline: 387 tables improve, 3,709 tie, and none regress. The sum of target settle ticks decreases from 414,032 to 405,336; maximum settle remains 10 ticks. These aggregate numbers are descriptive; acceptance is per table and per metric, with no compensating score. All 2,744 prior nonconstant nine-metric caps are preserved. Additional setup checks use the actual previous phase address: no metric regresses and no driver/lamp glitches occur.

Four player unit regressions pass. The full decoder suite passes 18 tests and retains one previously established failure at `tests/torch_row_decoder.rs:197` (actual 10 versus expected 8); its assertion is unchanged. Native/fresh Yosys source tests are extended to cover positive a, positive b on the final output, and the OR of two positive literals. Replay and final qualification metadata are recorded separately before commit.

The complete repeat is byte-identical to the first corrected family. All 13 native/fresh Yosys source cases pass. Against shipping: 3,162 improvements, 70 ties, 837 regressions on correct controls, and 27 recovered incorrect controls; automatic selection remains disabled.
