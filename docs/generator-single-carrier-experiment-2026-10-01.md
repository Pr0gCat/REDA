# Single-carrier input stages

Fixed certified control:9779496, generator-polarity-sum-experiment family.
The original shipping control is retained byte-for-byte in every record.

Short matrices restored each positive row locally but also retained a source
repeater. Removing that redundant source stage improves the physical path.
The generator now keeps the source stage only for the wider dual-arm geometry
or complementary multi-input cubes with unequal positive-literal counts.
This is a condition on generated control structure, independent of names and
function encoding; it also applies when the cubes feed separate outputs.
For a direct carrier, the source margin shrinks from four cells to three so
the farthest tap retains nonzero signal strength. Wider buffered layouts retain
their prior source geometry and matched restoration stages.

Rejected experiments: removing every source buffer failed internal truth at
[5,3,3,0] in the dual-arm layout. Preserving wide buffers alone failed internal
truth at [0,4,5,3]: the four-row direct carrier could not reach its farthest tap.
Moving that carrier one cell closer restored full4096 truth, but79 tables
regressed in retained-history settling, e.g.[1,0,0,1] went from10 to22 ticks.
These tables contain complementary full cubes with unequal inversion depths.
Protecting only shared-output reconvergence left18 regressions; separate-output
[2,0,0,1] is also a witness. Protecting the complementary source structure itself
removes every regression. Retained waveforms prove the isolation requirement;
this experiment does not separately instrument torch burnout counters or claim
that their contribution has been isolated. No extra holds or reset history.

Final validation:4096 source functions, strict physical certification and every
implemented signal in32 continuous setup/target phases all pass. Each of twelve
target metrics is compared separately against the frozen certified control:
2233 improve,1863 tie,0 regress. All2744 immutable caps pass; setup comparisons
have0 regressions and0 driver/lamp glitches. The16 player tests,77 Native/fresh
Yosys source fixtures and5 physical-rewrite consumer tests pass. A second full
run is byte-identical, SHA256
811298ce6d8ea027ef2eaa56dfd31fa8e4c9427a23f6877d0c4ee9df8ab166e2.
Producer exit1 is expected from27 unchanged incorrect shipping controls; all
candidate arms are correct and certified. Simulator and verifier are unchanged.

Automatic default selection remains disabled. The diagnostic target-dominance
policy now chooses4032 player and64 shipping tables, reducing retained-cap
conflicts from139 to15. This still rejects that policy. It has not yet checked
setup metrics and is not a qualified default implementation. Remaining work
must resolve the shipping tradeoffs and qualify default selection and fallback.

Reproduction: run mixed_decoder_tap_probe --shipping-control twice with
RUST_MIN_STACK=67108864, compare raw complete output with cmp, then run
compare_shipping_matrix.py against the frozen polarity-sum family using
--candidate-control. Use probe_default_selection_tradeoff.py for the separate
unqualified policy diagnostic. The summary and compressed full waveforms retain
this round's evidence; block count and volume are not acceptance criteria.
