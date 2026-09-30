# Mux delay regression comes from planner fallback

Production reference d5dba9f; replayed candidate is the already rejected
stage-preserving weak endpoint patch. Both runs use the same native source,
lowered graph and full from/to history. The new diagnostic records all actual
gate drivers, routed repeaters when a verified ledger exists, the output lamp,
planner selection and an independently attempted plan's refusal. When planning
fails, world repeaters are explicitly unattributed: no fictitious route owner
or certificate is assigned to the Legacy world.

The rejected candidate selects Legacy. Its independent typed plan fails with
NoLocalRoute from (40,1,35) to (41,1,31). Therefore the Mux quality regression
previously recorded is not a measurement of successfully emitted weak-terminal
routing. It is a measurement of the different fallback implementation. This
changes the next investigation from weak-dust update order to access-space and
routing failure. Weak-dust timing is still an independent qualification concern,
but cannot be inferred from this Mux failure.

For retained-history target 1->0:

| Observation | Stock Unified3d | Rejected Legacy |
| --- | ---: | ---: |
| n3 inverter goes true | 2 | 4 |
| n4 NOR goes false | 4 | 8 |
| g0 output merge goes false | 6 | 12 |
| Output lamp goes false | 10 | 16 |

The excess accumulates before the lamp. Legacy's observed relay events occur
at 2, 6, 10, 12 ticks at the recorded world positions. These observations do
not establish individual relay ownership; they must not be represented as a
successful weak-terminal path. The output-driver/lamp traces for all 64 targets
match the separately archived candidate and restored-control records exactly,
and both world fingerprints match those records. No independent cap is
regenerated or weakened to accept the candidate.

Each arm covers 64 setup and 64 target phases, checks all internal gate drivers
against the lowered Boolean graph and the lamp against an independent Mux
function, and retains physical history. Each world compiles twice with identical
fingerprints. Each complete diagnostic was repeated with identical stdout and
successful exit, including the explicitly reported candidate plan refusal.
This is diagnostic success, not candidate acceptance or Minecraft certification.

The experiment patch was withdrawn again: planner.rs and routing.rs are
byte-identical to HEAD. No production generator/simulator/test changed. The
new example and frozen traces are the only new implementation/evidence. The
next endpoint/layout candidate must reserve real access space and prove it
ships the intended planner's world before attributing quality to the new route.
The ongoing optimization goal remains active; automatic torch-row selection
remains disabled.

Reproduce stock after source tools/env.sh:

    RUST_MIN_STACK=33554432 cargo run --release --example mux_route_timing_diagnostic

For the archived experiment, apply its patch in an isolated checkout; preserve
its refusal and planner kind in the report, then restore the patch. The companion
JSON contains complete phase histories and route/world details for both arms.
