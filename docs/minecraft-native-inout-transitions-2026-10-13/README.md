# Top-level shared-inout four-state contract: exhaustive transition + waveform qualification

This qualifies the top-level shared-inout net-resolution contract (the four-state
`(value, present)` model, "B") end to end in actual Minecraft 26.2: every declared-state
transition, every electrical class, and continuous waveform convergence, on two independent
netlists, checked against an independent oracle. It closes the top-level external
drive / Z (Floating) / contention (X) sub-contract. It does **not** close gap 5 (Native
HDL) or any broad row — see [Limits](#limits).

Public entry: `frontend::compile_systemverilog_physical`. A structurally identified
top-level `inout` port selects the shared-channel semantic/materialization path
(`resolved::compile_for_materialization` → `compile::resolved_circuit::compile`); ordinary
sources keep the existing native path. The netlist-only `compile_systemverilog` entry
stays closed to top-level inout by design (asserted in `resolved.rs`
`hierarchy_driver_set_matches_frozen_four_state_oracle`); that boundary is correct and is
not removed by this work.

## Contract

Each resolved bus bit carries `(value, present)`. A top-level bit contributes an explicit
external caller pair `(external_value, external_present)`. A valid observation returns a
Boolean; Floating (`Z`), Contention (`X`) and protocol mismatch remain explicit errors —
they never become source Boolean success.

## Method

- **Exhaustive transition walk (M1).** The 64 declared states form a complete digraph with
  self-loops, which is Eulerian. A Hierholzer circuit (`fixture*/schedule.py`) produces one
  4097-vertex walk that traverses all 4096 ordered `(before → after)` transitions exactly
  once. Representatives are chosen `protocol_matched` (`caller == present && declared`) so
  the walk lands in every Value/Contention/Floating class instead of masking 16 states as
  ProtocolViolation.
- **Continuous waveform convergence (M2).** `poll_until_quiescent` reads lamps, gates and
  relays on every poll, so the raw RCON log already contains the full per-poll waveform.
  `fixture*/waveform-audit.py` reconstructs the driver's exact per-poll `read_state` dicts
  from the log (marker-pair grouping on the first lamp read, which is queried twice), then
  requires the maximal identical **suffix** of the poll sequence to be at least
  `REQUIRED_STABLE_POLLS = 5`. A redstone transient can pass through the final value
  mid-settling, so convergence is the final fixed point, not the first match. This is the
  same guarantee the frozen protocol enforces — no extra settling allowance.
- **Independent oracle.** A pure-Python multi-driver resolution reference
  (`docs/inout-standard-oracle-2026-10-04/run.py`, `fixture2/gen_oracle.py`) generates the
  8192-row `(before, after, phase, value)` expectation. It is independent of the Rust
  compiler and uses no HDL simulator.
- **Frozen game protocol.** `circuit_conformance.py` (`run`, `poll_until_quiescent`,
  `read_state`, `read_relay_state`) is frozen and shared by both runs. The driver only
  monkeypatches `read_state` (adds channel / readback / external-pad reads) and
  `read_relay_state` (bounded re-forceload retry on a transient `RegionNotReady` — an
  environment retry, not a settling change).

## Results

| Check | Fixture 1 (37-gate writer) | Fixture 2 (35-gate inverted writer) |
| --- | --- | --- |
| Transitions exercised | 4096 / 4096, 0 missing | 4096 / 4096, 0 missing |
| Declared states observed | 64 / 64 | 64 / 64 |
| Checked classes (complete raw reads) | Value 38 / Contention 18 / Floating 8 | Value 38 / Contention 18 / Floating 8 |
| Electrical failures | 0 | 0 |
| Settles converged | 4098 / 4098 | 4098 / 4098 |
| Bad waveforms | 0 | 0 |
| Unparseable log lines | 0 | 0 |

Fixture 2 keeps the same 64-state space and port contract but is a different netlist
(inverter, gate count 35) with a different state→class mapping; the aggregate class counts
coincide by bijection while the per-state expectations differ — that is exactly what the
method must generalize over. The two fixtures use different world origins
(`230000,151,180000` and `231000,151,180000`).

## Reproduce

The raw RCON logs (3.53 GB / 3.21 GB) and per-vector results (12.7 MB / 12.4 MB) are
retained on disk under `target/qualification/` with the SHA256 recorded in
[`manifest.json`](manifest.json); they are not committed due to size. With those files
present, re-run `fixture1/transition-audit.py` and `fixture1/waveform-audit.py` (and the
`fixture2/` copies) to reproduce every accepted result. `fixture*/schedule.py` regenerates
the walk; `fixture*/run-transitions.py` re-runs the game walk against the frozen protocol.

## Limits

This qualifies the top-level shared-inout four-state contract for the specified binary
valid domain (Value / Contention / Floating / ProtocolViolation) across all 4096
declared-state transitions and continuous waveform convergence, on two independent
netlists, against an independent oracle. It does **not** close gap 5 (Native HDL) or any
broad row. Still unqualified within gap 5: operators beyond the integral subset
(relational, shift, division, reduction), unsized literals, `int`/`integer` signals,
non-integral and >64-bit parameter types, >32-bit constant arithmetic, and the
128-level hierarchy-depth and no-output-module rejections (no tests).
