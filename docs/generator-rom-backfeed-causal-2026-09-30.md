# Current ROM backfeed attribution and retained-history intervention

Production reference: 3aba78b. The previous goal turn made progress by submitting
a downstream timing-cone correction. This turn revalidates the separate stock
ROM routing defect against the actual current production compiler; no older
failure is assumed still present.

The diagnostic now attributes powered dust on a logically false input using
the verified current route ledger. For each bare landing it reports all sibling
receiver terminals, their owners, logical values, physical state, and coordinates.
The old fixed-coordinate route dump was removed. An optional causal-cut mode
removes only active foreign repeater terminals whose logical source is true.
It mutates the existing Simulator after the observed failure, preserving torch
history. It always exits with a failure after an intervention: deleting a required
input is not a candidate implementation or passing physical acceptance.

For [5,3,2,0], the first failing phase is from=0,to=0,address=0. g3 should be true
but is false. The n0 source is false, yet its dust at (31,1,37) has power 15.
The sibling g0 repeater into n1 at (33,1,37) is on and its logical value is true.
It strongly powers the shared support, which repowers the n0 dust. The route's
repeater into g3 then propagates that false signal and strongly powers another
support, repowering n3's dust as a secondary consequence. Cutting only the g0
landing (not the secondary repeater carrying an already-corrupted false signal)
restores g3 from false to true after 5 ticks, without resetting physical history.
This is causal evidence for foreign terminal drive, not a shipping fix.

The current [4,1,0,2] and [4,3,2,5] diagnostics both pass all 32 setup/target
phases, checking every gate driver and output lamp. Their cut modes perform no
intervention because no failure occurs. Current evidence therefore does not
support continuing to describe all three old cases as still broken. These two
results do not certify the full 4096-function family or prove a causal connection
to any particular earlier change.

Each of four invocations (uncut/cut [5,3,2,0], cut-mode for the other two) was
repeated. Stdout and exit status match exactly. The companion JSON preserves
observations and production reference. Uncut/cut failures exit 101; the other
two exit 0. The example compiles successfully and git diff --check passes.
No production generator, simulator, acceptance tests, metric caps, or observation
history changed in this turn. The active optimization goal remains unfinished.

The next general endpoint candidate should preserve existing repeater stages
while making shared multi-input support landings electrically weak. Uniform
weak terminals and universal new buffer stages already have documented fixed-cap
regressions; neither is accepted. A typed router experiment must reserve the
required terminal geometry, retain relay metadata and placement ownership, and
pass fixed controls plus complete history truth. The explicit side-torch ROM
implementation remains available, but automatic stock/row selection is disabled.

Reproduce after source tools/env.sh:

    RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 5,3,2,0
    RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 5,3,2,0 --cut-strong-landing
    RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 4,1,0,2 --cut-strong-landing
    RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 4,3,2,5 --cut-strong-landing
