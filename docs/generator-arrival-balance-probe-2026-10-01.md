# Nominal arrival balancing exposes an ineffective refresh (diagnostic only)

Reference: 105c459. This round replays the archived required-refresh weak
endpoint generator solely to investigate timing. A generic diagnostic sums
upstream gate delays and real route relay delays, propagates the latest nominal
arrival, and optionally adds delay-1 relays to earlier branches at unique,
straight, solid-backed dust cells. Placement depends on connectivity, recorded
paths and relative arrival, not fixture names. Names select only the manual
Mux/XOR/fresh-Yosys-AND source fixture. It refuses missing insertion geometry.

The probe mutates an emitted world after its original route/world equivalence
check. These added relays have no new certified route guard or updated generator
ledger. The final report names every mutation, reports final and original
fingerprints separately, and only claims a verified final ledger when an actual
plan exists and its emitted-world fingerprint equals the final world. This is
not an accepted compiler optimization or substitute for typed integration.

## Local AND result

One probe relay is added on the n2 branch at (50,1,29), matching the n5 branch's
existing two-tick refresh. Every internal gate and independent conjunction lamp
is correct in 256 setup plus 256 target phases with retained physical history.
All output pulses disappear. Measured results match the independent stock
AND record: world worst 22 ticks, total target settling 1465, driver/lamp worst
changes 8/12, and zero driver/lamp glitches and pulse widths. The diagnostic
uses least-significant to most-significant a,b,c,d, matching its separately
frozen stock record in generator-required-refresh-rejected-2026-10-01.json;
the fixed frontend acceptance suite's differently ordered profile is unchanged.
No full fixed-suite pass for the mutated probe world is claimed.

## XOR rejects the general estimate

Two nominally early XOR branches get probe relays, but all 16 target plus 16
setup phases still give driver/lamp glitches 2/1 versus current checkpoint 1/0.
Driver/lamp total pulses are 4/5 versus 2/0, worst changes 8/12, and world worst
12, total target settling 114. This fails individual glitch and pulse metrics;
AND's local result cannot compensate for it. No size metric ranks acceptance.

New observers cover each actual terminal in addition to every gate and relay.
For XOR target 3->0, landing b->n2 changes false at tick 1, while the recorded b
refresh at (26,1,33) changes false at tick 2. The nominal path incorrectly charges
that input two ticks of refresh. Its recorded consecutive anchors are
(26,1,34) -> (26,1,33) -> (25,2,33). The existing route_step_is_legal predicate
returns false for this repeater step: its direction does not serve that recorded
continuation. The original plan/world fingerprint check passing does not prove
this individual claimed refresh is effective.

A causal cut replaces only this relay with Air in the same candidate initial
world, then runs the complete retained history without any resets or longer
hold. All 32 setup/target phases retain correct every-gate and independent XOR
lamp truth; their ticks and every other gate, landing and relay observation are
identical to the uncut probe. Only the removed relay's events disappear. This
shows it does not provide the claimed arrival delay for this measured history.
It does not certify deleting it universally or determine every possible bypass
path. The same owner's two wire branches include adjacent cells, so nominal
branch membership alone is insufficient to prove refresh dominance.

The general balancing experiment has not qualified. Next, actual conductor
connectivity and legal relay direction must establish which refreshes are on
all effective delivery paths before they contribute to timing estimates. A
path-count model cannot replace real endpoint observations and simulator truth.
No production cleanup based only on block counts or this one cut is accepted.

Each of the three complete probe runs was repeated with identical stdout and
exit status. Each base world compiles twice deterministically. The companion
JSON retains full 512/32/32 phase observations, terminal/relay geometry, nominal
estimates, and scalar metrics. The original controls remain untouched. The
archived generator patch was withdrawn again: planner.rs, routing.rs and
relax/build.rs are byte-identical to HEAD. The unchanged frontend/generator
quality suites pass all 1312 measured transitions after withdrawal. Only the
manual diagnostic extension and evidence remain new. No simulator, test cap,
physical history or acceptance interval changed; the optimization goal remains
active and automatic torch-row selection is still disabled.

Reproduce in an isolated checkout of the archived generator experiment, after
source tools/env.sh and with RUST_MIN_STACK=33554432:

    cargo run --release --example mux_route_timing_diagnostic -- and4-yosys --balance-arrivals
    cargo run --release --example mux_route_timing_diagnostic -- xor2 --balance-arrivals
    cargo run --release --example mux_route_timing_diagnostic -- xor2 --balance-arrivals --cut-relay=26,1,33

The explicit-coordinate cut is a causal diagnostic, not the optimization rule.
Restore the generator patch before accepting any shipping change. No complete
repository suite, complete circuit family or Minecraft certification is claimed.
