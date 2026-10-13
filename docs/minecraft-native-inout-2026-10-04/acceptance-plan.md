# Shared-inout (B) acceptance plan

Goal: close the broad shared-inout row by meeting the immutable acceptance
rules (`../generation-gap-audit-2026-10-03/status.md:54`) for the top-level
inout contract B (four-state net resolution). B is implemented and verified at
source/semantic/consumer/terminal level, and its settled-state real-game result
is reproduced with the current compiler. Three required items remain open
(`README.md:13-16`). This plan closes them.

## Acceptance bar (unchanged, authoritative)
Observe the actual generated circuit AND continuous waveforms; root-cause;
general method; independent comparison; adopt only after complete acceptance.
Bans: no block-count/volume acceptance, no simulator/physical-rule weakening,
no deleted tests, no new ignores, no repinned controls, no extra settling
allowance. Retain all public occurrences/constants/aliases/observations/
determinism/consumer contracts.

## What is already green
- Actual generated circuit: 37 gates, 12 lamps, channel/readback/external pad,
  38 Value / 18 Contention / 8 Floating / 64 ProtocolViolation, independently
  audited against the frozen 8192-row HDL oracle. Reproduced with HEAD
  `101a4cd` (producer SHA256 match; `final-independent-audit-current.json`
  accepted True).
- General four-state method: verified against an independent oracle at source,
  semantic, consumer, terminal, compiled-circuit level.
- QoR characterized (`qor.py`): footprint 170x7x99, 3282 non-air, density
  0.0279, 51 lowered gates, per-state settle median 3.57s. NOT an acceptance
  basis (rule: no volume acceptance) — recorded only.

## Required items (the open gap)

### M1 — all game transitions
The current sweep checks each state at rest; it does not verify the circuit
settles correctly from an arbitrary prior state. Drive every ordered
state-to-state pair among the 64 declared states (64x64 = 4096). For each
(from -> to): settle at `from`, drive to `to`, settle, verify the full signal
set at `to` equals the oracle's `to` row. Catches stuck/hazard/state-dependence
that a settled-only sweep misses. The known adjacent-transition defect
(`README.md:47-50`, `0110100 -> 0110101`) is covered because quiescence here is
full-signal (lamps + gates + reader repeaters), not gate-only.

Note: the README names "4096" without defining it; 64x64 is the only reading
that fits the 64 declared states. If the intended set differs, this is the
assumption to correct.

### M2 — continuous waveform guarantee
`poll_until_quiescent` returns only the fixed point and discards intermediate
polls. Capture every poll's full-signal reading during each settle (the
waveform trace). Verify each trace reaches a fixed point and holds it: no
oscillation past settling, no settled output glitching, convergence within the
dump-derived ceiling. This is the "continuous waveforms" the rule requires; the
existing `run-reader-wave.py` is a sampled diagnostic, explicitly "not a
complete tick waveform" (`README.md:56`).

M1 and M2 share one run: the transition driver samples every poll, so the same
4096 settles yield both the transition results and the waveform traces.

### Method as implemented (fixture 1)
- Walk: Hierholzer Eulerian circuit over the 64 declared states (complete
  digraph with self-loops) -> one 4097-vertex walk covering all 4096 ordered
  pairs exactly once. Vectors emitted in `sorted(inputs)` order (the driver maps
  vector bits to sorted input names).
- Representatives: each declared state is realized by a PROTOCOL-MATCHED control
  setting (caller == present&&declared), so the walk exercises every state in
  its real electrical class (Value/Contention/Floating), not a caller-mismatch.
- M1 audit: reconstruct per-state observations from the raw RCON log; every
  landing state's full signal set + checked_kind must match the frozen trace;
  all 4096 edges must appear as consecutive vertices of the executed walk.
- M2 audit: reconstruct each settle's full-signal poll sequence; the fixed point
  is the maximal identical SUFFIX of the sequence and must be held >=
  REQUIRED_STABLE_POLLS (5). A transient that equals the final value mid-settle
  is not a violation. (An earlier first-match check produced 399 false
  "post-settling change" reports; corrected to the final-suffix criterion.)
- One RCON log line carried a transient non-UTF-8 byte (log-integrity glitch,
  not a circuit observation); audits tolerate it (errors="replace", skip + count
  unparseable lines).

### M3 — general, not one fixture
Physical evidence is one fixture. Add a second shared-inout fixture with a
different topology (different driver/contention/floating structure), produce it
with the same public API, and run M1+M2 on it. Proves the method generalizes.

Staged offline this session: second fixture
`target/qualification/shared-inout2-producer/fixture.sv` (one writer drives the
INVERTED data, so the per-state Value/Contention/Floating mapping and the
generated netlist differ from the first; same top-level-inout contract) and its
independent oracle `gen_oracle.py` -> `standard-oracle.csv` (64x64x2 = 8192
rows), a pure-Python multi-driver resolution reference — the same method as
`docs/inout-standard-oracle-2026-10-04/run.py`; the first fixture's oracle is
also a Python reference model (`standard_simulator_evidence: false`), not an HDL
simulator (none installed). The second exporter `src/main2.rs` is built and run:
it compiles the second fixture through the public API (gate count 35, 2
injections, classifications Value 38 / Contention 18 / Floating 8 /
ProtocolViolation 64 — matching the independent oracle; the producer's own
electrical-consistency assertions pass for all 128 raw states). The second run
dir `target/qualification/shared-inout2-transition-run/` holds the dump, trace,
4097-vertex schedule, driver and audits. The second M1+M2 game run
(origin 231000,151,180000) + audits are COMPLETE and accepted (see Outcome).

## Cost / risk
- M1+M2 game run: 4096 transitions x ~3.6s settle+sample ~= 4h wall-clock per
  fixture, plus flake retries (one transient RegionNotReady seen; clean re-run
  is the response). M3 doubles it.
- Waveform sampling raises RCON volume (every poll, every signal). Log size
  grows; audit must stream.
- Risk: a transition that settles to the right value but glitches a settled
  output mid-trace would fail M2 even though M1 passes — that is the point of
  the waveform guarantee, not a harness bug.

## Order and verification
1. Build the transition+waveform driver (offline, reuses frozen protocol).
   Verify offline: schedule = 4096 pairs, waveform capture logic, oracle binding.
2. Run M1+M2 on the existing fixture. Audit: every transition's final state
   matches the oracle; every waveform converges. No new ignores, no weakened
   settling.
3. Build + run M3 (second fixture). Same audit.
4. Only if all green: remove the prototype guards (ordinary-path rejection
   `elaborate.rs:211`, CLI restriction `build_circuit.rs:795`) and mark the
   row closed. Until then B stays "implemented + settled-state verified".

## Outcome (2026-10-13) — accepted
All three required items are green on two independent fixtures. Retained evidence
and reproduction instructions: `../minecraft-native-inout-transitions-2026-10-13/`.
- M1: 4096/4096 ordered transitions exercised exactly once (Eulerian walk),
  64/64 declared states observed, checked Value 38 / Contention 18 / Floating 8,
  0 electrical failures, 0 unparseable log lines — on both fixtures.
- M2: 4098/4098 settles per fixture reach a fixed point held >= 5 consecutive
  identical full-signal polls within the dump-derived ceiling; 0 bad waveforms.
- M3: second fixture (35-gate inverted writer, different origin and state->class
  mapping) passes M1+M2 with the same method and an independent oracle.

Step 4 correction: the "prototype guards" are NOT removed. `elaborate.rs:211`
(ordinary netlist path rejects top-level inout) is a test-pinned correctness
boundary — `resolved.rs` `hierarchy_driver_set_matches_frozen_four_state_oracle`
asserts `compile_systemverilog` must stay closed to top-level inout; the shared
contract is served by `compile_systemverilog_physical`. `build_circuit.rs:795`
only rejects shared-inout combined with `--grown/--synth/--pins/--evaluation`
(paths that do not implement it). Both are correct and remain. Adoption is the
status record, not a code change: the current claim is appended to
`../generation-gap-audit-2026-10-03/status.md`. Gap 5 (Native HDL) stays open —
its other items (operators beyond the integral subset, unsized literals,
int/integer signals, non-integral/>64-bit parameter types, >32-bit constant
arithmetic, hierarchy-depth/no-output-module rejections) remain unqualified.
