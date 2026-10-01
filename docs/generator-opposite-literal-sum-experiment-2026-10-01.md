# Shared positive sums with opposite literals

Fixed certified control:758c2c6, generator-single-carrier-experiment family.
The original shipping control remains exact in every record.

The remaining shipping tradeoffs all concerned total target settling, not
worst-case settling. One structural family combines positive OR with private
opposite-polarity unary outputs and optional positive aliases/constants. The
old common-polarity sum recognizer refused this family, leaving every OR row
with two restoration stages. Recognition now uses cover structure: positive
unary OR terms, unary outputs of either polarity and constants. Only the OR
path uses a single restoration stage. Opposite unary branches retain their own
input repeater and torch, mounted on a private lower support. Repeated opposite
unary outputs retain the existing qualified matrix because they still need
separate output terminal geometry. No function-name or encoding whitelist.

The first direct-inverter prototype passed4096 truth/physical/internal checks
and was deterministic, with54 improving tables, but6 regressed specifically in
driver last-change ticks. Example[4,7,1,3] reduced total settling84 to64 while
its driver last-change rose4 to6. It was rejected. Restoring the private unary
stage preserves that driver's4-tick limit and reduces total settling84 to72.
For[2,3,1,1], total settling79 becomes67 without worse per-metric timing or
pulses. Private support and planar repeater-to-dust handover are necessary:
sharing a primary floor violated placement ownership, direct lamps lacked the
required route description, and the first elevated handover was physically
disconnected. Those rejected geometries did not relax the verifier.

Final validation:4096 functions all pass source equivalence, strict physical
certification and internal implemented truth over32 continuous setup/target
phases. Twelve target metrics are compared separately:60 improve,4036 tie,
0 regress. All2744 immutable caps pass; setup has0 regressions and0 driver/lamp
glitches. All60 supported output combinations have retained-history unit
coverage;17 player tests,77 Native/fresh-Yosys fixtures and5 physical rewrite
consumer tests pass. Second full output is byte-identical; SHA256
1ed5062d756ac5ad85c8e112ef856869756d54e97599647934cd60419897da8b.
A Rust/LLVM compilation crash was retried with RUST_MIN_STACK=134217728 as
suggested by rustc, and the original release tests then passed. No simulator,
verifier, history resets, added holds or caps changed. The producer's exit1
continues to reflect27 unchanged incorrect shipping controls.

Automatic compile selection is still disabled. The separate prospective
shipping-dominance diagnostic chooses4050 player and46 shipping tables, with
9 retained-cap conflicts (previously15). Six of the remaining conflicts concern
opposite unary output aliases; three concern mixed complementary cube outputs.
It has not checked setup and is not a qualified policy. Remaining generation
and default/fallback qualification are still required.

Reproduce full family/repeat with mixed_decoder_tap_probe --shipping-control,
compare raw files with cmp, compare_shipping_matrix.py against the fixed
single-carrier family with --candidate-control, and the unchanged prior caps.
See the summary and archived complete waveforms for final evidence.
