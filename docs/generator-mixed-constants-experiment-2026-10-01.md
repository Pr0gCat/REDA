# Mixed shared-support products with constant outputs

Reference `c7381eb`. Observe all 24 additional combinations containing a
positive two-literal product, a negative single literal, and a low or high
constant. The previous generator falls back to its buffered matrix for these
combinations; all have worst settling 10 ticks in the immutable control.

Extend the existing cover-based recognizer to identify a constant port when
it has no variable term. Preserve the requirement for both a product and a
negative literal. Generate ordinary high/low torch drivers in a separate
conductor domain, beyond all variable rails. A shared helper retains exactly
the previous product-only constant geometry; existing mixed variable rails
and public terminal spacing remain unchanged. No table encoding chooses the
implementation and no new electrical permission is introduced.

Full 4096 comparison against the unchanged certified `c7381eb` family:
24 improve, 4072 tie, zero regress on any of the twelve independent waveform
metrics. Every candidate is certified and physically correct in all 32
continuous setup/target phases. All raw shipping controls remain exact.
Exactly those 24 improved cases change their emitted world fingerprint;
all other 4072 worlds are identical to their controls.

For `[2,0,2,1]`, worst settling improves 10 to 8 ticks, total target settling
99 to 59, driver last change 6 to 4, and lamp last change 10 to 8. Driver/lamp
glitch and wrong-pulse metrics remain zero. The 2744 independent prior caps
pass unchanged; setup metrics do not regress and setup glitches remain zero.

The complete repeat is byte-identical, SHA256
`4b9293e47c2e4f2e0b9b4586572567bed2ea647dbb4e40d005aedac6bd5ccb54`.
Producer exit 1 still comes from 27 incorrect immutable shipping controls,
not the corrected candidate arm. The comparison command passes.

All 15 player tests pass, including all 42 mixed combinations and the prior
product/constant, literal, fanout and wide-cover regressions. All 41 Native
and fresh-Yosys source cases pass, with source equivalence and deterministic
certified physical implementation. No simulator, hold/history behavior,
acceptance cap or verifier policy changes. This remains an explicit player
generator; default selection and broader reconvergence work remain pending.
