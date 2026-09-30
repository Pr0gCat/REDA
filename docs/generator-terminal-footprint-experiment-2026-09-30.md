# Directional weak-terminal access footprint (rejected)

Reference production commit 1c616cc. The previous experiment's Mux had fallen
back to Legacy after NoLocalRoute. Placement currently includes each NOR input
socket, but not the upstream private relay approach or its floor. The generic
candidate extends the occupied/access geometry of multi-input torch bodies on
each actual rotated input face: hops 2 and 3 carry that input's signal, and the
relay floor at hop 2 is inert. It does not add a uniform spacing increment to
unrelated bodies. The prior demand-based weak-terminal routing patch is part
of this experiment; every selected relay has its real direction, reservation,
terminal kind and path metadata. No case/module name chooses this policy.

An initial temporary runway trace records the forced cells and their current
claims. It does not identify a foreign block occupying all these cells: the
recorded intermediate claim is the route's own conductor and the outer cell
is unclaimed. Therefore the candidate tests an incomplete access-space model,
not a proved single foreign-block collision. The trace instrument was removed
before the candidate tests; its evidence is retained in the JSON.

With the directional footprint the real Mux compiler selects Unified3d, its
independent plan succeeds, and emission is world-identical to stock compile.
All internal gates and the independently expected Mux lamp are correct over
64 setup plus 64 target phases with retained simulator history. Compilation
fingerprints and repeated complete stdout are identical. For target 1->0 the
internal n3/n4/output/lamp sequence is 2/4/6/10 ticks again, versus the earlier
Legacy fallback's 4/8/12/16. This demonstrates a usable access-space candidate,
not complete physical quality acceptance.

Complete histories expose a second problem that the single target obscures:

| Mux metric | Independent restored control | Footprint candidate |
| --- | ---: | ---: |
| Driver worst change | 8 | 8 |
| Lamp worst change | 12 | 12 |
| Driver glitch transitions | 6 | 10 |
| Lamp glitch transitions | 4 | 6 |
| Driver total pulse ticks | 16 | 24 |
| Lamp total pulse ticks | 12 | 22 |

Single-input driver glitches remain 1; single-input lamp glitches remain 0.
Driver/lamp longest pulse remains 4/5 ticks. Candidate worst settling is 12,
total target settling 424. Restoring one target's speed cannot excuse additional
multi-input pulses, and counts/volume play no part in acceptance. The companion
JSON preserves complete Mux observations and derived metrics.

The independent frontend suite passes all 1024 measured transitions, but the
separate matrix rejects xor2/native at its unchanged current lamp-glitch cap:
1 > 0. Its driver glitches are also 2 > 1. Candidate driver/lamp worst changes
are 6/10 versus checkpoint 8/12; driver total pulses are 4 versus 2, lamp total
pulses 5 versus 0. No faster metric compensates for these regressions. Raw XOR
boundary traces/fingerprint are frozen with the failed experiment.

The [5,3,2,0] ROM obtains an exact verified ledger/world and all 32 setup/target
phases have correct gate drivers and lamps. This is a local functional result,
not a nine-metric ROM improvement or full-family qualification. No later matrix
cases are represented as passing after the XOR assertion stopped the suite.

The complete failed planner/router/footprint patch is archived, and all three
production files were restored byte-for-byte to HEAD. No test, independent cap,
physical history, simulator rule, or acceptance interval changed. Automatic
ROM torch-row selection stays disabled. The remaining problem is physical
branch timing/hazards under complete simultaneous-input histories once the
new endpoint can actually route; layout access alone is insufficient. The
optimization goal remains active.

The complete candidate/control internal traces localize two new lamp pulses at
2->4 and 2->5. Both n0 and n1 drivers change at tick 2. The n0 route's relay
changes at tick 4; n1's route has no relay. Candidate n2 changes true at 4 and
false at 6; its output relay and g0 change true at 6 and false at 8; the lamp
changes true at 7 and false at 12. The independent stock histories have no
n2/g0/lamp changes on these targets. This identifies an unequal physical input
arrival as the next endpoint experiment's target, not a reason to change the
simulator update ordering or observation hold. A balanced local prototype is
already documented separately, but its earlier universal integration failed
quality caps; these traces do not authorize accepting that rejected solution.

After rollback, frontend and generator quality suites pass all 1312 measured
transitions again, and planner.rs, routing.rs and relax/build.rs are each
byte-identical to HEAD. git diff --check passes. Reproduction uses
mux_route_timing_diagnostic, rom_output_history_diagnostic with 5,3,2,0,
frontend_quality_report and generator_quality_matrix under source tools/env.sh
and RUST_MIN_STACK=33554432. No full repository or ROM-family success is claimed.
