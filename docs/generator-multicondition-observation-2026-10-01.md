# Mixed conditions: next generation bottleneck

Reference `fd4d83f`. Read all 4096 frozen family records without regenerating
shipping controls. Among controls that are physically correct, 714 candidate
tables have larger total target settling time. This is a settling-only diagnostic,
not a complete quality comparison or an acceptance score. The companion JSON
records the source hash, structural grouping, and eight largest witnesses.

The largest witness has words `[6,4,2,1]`: outputs are `a AND b`, `NOT a`,
and `NOT b`. The current `build_candidate` recognizes a single shared product
or disjoint unary literals, so this combination falls through to the generic
matrix. `needs_input_buffer` applies a common entry stage because the positive
product uses inversion taps. `player_logic` then adds private mismatch stages
and data stages for the unary outputs as well.

Replay the actual certified candidate with internal observations:

    source tools/env.sh
    RUST_MIN_STACK=67108864 cargo run --release --example mixed_decoder_tap_probe -- --shipping-control 678 679 --internal

All 32 continuous setup/target phases pass. Removing only the additional
internal event labels reproduces the frozen candidate phase records, including
all public events, exactly. For target 0->3, both source repeaters change at
tick 2; mismatch taps change at tick 4; all three public drivers change at tick
6; the rising lamp changes at tick 7 and falling lamps at tick 10. Candidate
worst settling is 10 versus the correct shipping control's 8; total settling
excess is 42 ticks across the 16 targets. The full internal replay is archived.

Next construction: recognize multiple output conditions structurally and reuse
the certified product and literal implementations in isolated electrical
domains. First establish how the shared primary signal reaches every domain
with certified 0/15 strength, actual one-way delivery and synchronized arrivals.
Duplicating floor levers would violate the source boundary; arbitrary strong
support or passive long fanout cannot substitute for certified input routing.
Outputs with multiple terms need independently certified reconvergence as well.
No production change or default compiler selection is accepted by this report.

Qualification remains full-family source equivalence, strict physical
certification, retained history, twelve separate per-table waveform metrics,
unchanged prior caps, exact controls, complete deterministic repeat, and
appropriate output/constant/DFF regressions. Block counts do not rank acceptance.
