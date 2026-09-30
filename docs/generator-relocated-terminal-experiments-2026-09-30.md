# Existing-stage weak landing experiments (both rejected)

Independent production reference: c0d0c21. The retained-history causal cut
identified a true foreign repeater into a shared NOR support as the source of
[5,3,2,0]'s n0 backfeed. Existing direct-repeaters are not inherently safe when
another signal lands on the same support as dust.

Both experiments use typed requirements on multi-input NOR receivers in
combinational circuits. They preserve merge, sequential and explicit-terminal
contracts. WeakAutomatic first selects the terminal as the existing Automatic
policy would; WeakRepeater retains the existing required stage. A selected
terminal repeater moves one cell upstream into private, straight dust, and the
last landing becomes directed dust. An already bare terminal adds no buffer.
A pre-existing predecessor repeater, shared relay cell or invalid geometry is
refused. Emitted cells, terminal kind, relay direction, guard reservation and
actual path repeater metadata reflect the relocation. Names and ROM data do
not select the rule. This attempts to preserve stages, not to rank by size.

## A: universal two-cell runway for eligible receivers

The fixed frontend suite passes (1024 measured transitions). [5,3,2,0] gets a
verified Unified3d ledger exactly matching the shipped world and passes all
32 setup/target phases with correct internal drivers and output lamps.

The separate generator matrix rejects xor2/native: worst lamp change is 16
ticks against frozen cap 14. [4,1,0,2] and [4,3,2,5] fall back to Legacy, and
the independent plan refuses NoLocalRoute respectively from (39,1,33) to
(40,1,29), and (35,1,40) to (31,1,37). No physical success for these refused
ledgers or complete matrix qualification is claimed. Requiring a two-cell
runway for even terminals that remain bare unnecessarily changes branch paths.
The first complete failed patch is frozen separately.

## B: runway only when relocation requires it

The normal search is attempted first. If a selected repeater cannot move onto
the private straight predecessor, the staged reservation/cell/floor/branch
state is rolled back and search retried with two runway cells. This preserves
normal dust paths where no relay relocation is needed. The fixed frontend
suite again passes, and both XOR frontends pass all matrix controls before the
next case fails. [5,3,2,0] and [4,1,0,2] pass all 32 setup/target physical phases
with verified ledgers. [4,3,2,5] still refuses the same NoLocalRoute, so it is
not a certified integration success.

Mux2/native rejects worst lamp change 16 against frozen cap 12. Complete
candidate observations were saved before the test asserted its unchanged cap.
Eight falling target transitions have driver change at 12 and lamp change at
16 ticks: 1/3 to 0/2/4/5. Candidate driver glitch count is 4, one single-input
case, total pulse width 8 ticks and longest pulse 2. Lamp glitches remain zero.
Correct truth or improving a different metric does not excuse the delay.
The demand-based patch and full candidate Mux traces/fingerprint are archived.

Both production files are restored exactly to c0d0c21. Neither test source,
cap, simulator rule, settling limit nor physical history was changed. The
rejected patches are evidence, not active code. No automatic ROM-row selection
is enabled. This cycle finds that stage-preserving relocation alone is not a
qualified solution: receiver access space and physical update timing remain
requirements for the next candidate. Universal new buffering and uniform weak
replacement remain previously rejected alternatives, not fallbacks.

Commands (source tools/env.sh first, RUST_MIN_STACK=33554432):

    cargo run --release --example rom_output_history_diagnostic -- 5,3,2,0
    cargo run --release --example rom_output_history_diagnostic -- 4,1,0,2
    cargo run --release --example rom_output_history_diagnostic -- 4,3,2,5
    cargo test --release --test frontend_quality_report
    cargo test --release --test generator_quality_matrix

After rollback both fixed quality suites pass all 1312 measured transitions.
The restored independent Mux record is retained beside the rejected candidate:
worst driver/lamp change is 8/12, driver glitches 6, lamp glitches 4. For the
eight rejected targets the restored driver/lamp traces are 6/10 (1/3 to 0/2)
and 4/8 (1/3 to 4/5), versus candidate 12/16 throughout. Thus the output driver
itself is delayed by 6 or 8 ticks; the lamp's unchanged four-tick falling delay
is not the cause. The next timing investigation must inspect the upstream
route/component stages, not adjust lamp simulation or its cap. Reduced glitches
(6 to 4 drivers, 4 to 0 lamps) are a tradeoff, not acceptance. Both restored
production files were checked byte-for-byte against HEAD; git diff --check
passes. No full repository-suite or full ROM-family success is claimed.
