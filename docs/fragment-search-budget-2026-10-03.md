# Real recursive proposal budgets

The public `compile_fragment_synth` entry now evaluates existing recursive composition alternatives. Previously every budget returned one baseline with zero evaluations and an empty trace.

The baseline is unchanged at zero budget. Positive budgets try dense packed leaves, then standard packed leaves, in a fixed order. Both alternatives use the existing recursive producer, exact pin normalization, leaf routing and whole-root certification. The legacy whole-circuit seed remains unreachable. The current stream contains two real root proposals; it does not implement arbitrary local fragment replacement.

Each completed proposal reports its terminal result and certified metrics. Work counters currently record completed root manifest pairs only; other `cap_work` fields are uninstrumented, and their zero defaults are not zero-work measurements. Replacement requires the shared retained-history qualifier: all source gates and public outputs must be correct, all setup/target quality metrics and glitch-transition counts must dominate, and every phase settlement and public channel last arrival must be no worse. A strict improvement is required. Block count and volume do not select a candidate. A composition refusal is counted, not represented as a successful world.

The qualifier now supports exact caller-owned ports. It checks that their shipped cells are empty, installs the existing caller lamp fixture only into the measurement copy, and drives pinned inputs through the existing external source fixture. Generated levers retain their original drive method. It never edits the emitted circuit or relaxes physical verification.

Native time budgets use a monotonic clock, checked before starting a proposal. A proposal already started can finish. Evaluation budgets do not read a clock. Browser WebAssembly time budgets explicitly return `TimeBudgetUnavailable`; host-clock integration remains unfinished. The WASM compilation check is not browser runtime conformance.

The recursive producer identity is versioned for the new search. All three historical v1 fingerprint constants and their checks remain; the current v2 identity differs and remains identical across budgets. Historical worlds and measurements were not repinned.

## Completed experiments

The independent Rust probe compiles the actual public API, exports a world named by its canonical fingerprint, reloads it and checks the fingerprint. It records every source gate output, every public driver/lamp occurrence, the power-on state and all observed events. An independent Python evaluator rebuilds NOR/OR truth, checks the complete protocol, replays every recorded event, rejects hidden transitions at those observed signals, and compares full phase/channel quality against the frozen library control.

| Source | Phases per world | Budgets | Fresh repeats per budget | Result |
|---|---:|---|---:|---|
| Two-gate chain | 8 | 0, 1, 2 | 3 | Original world retained |
| Exact pinned two-gate chain | 8 | 0, 1, 2 | 3 | Original world and ports retained |
| Four-input AND | 512 | 0, 1, 2 | 3 | Original world retained |
| Three-input Mux | 128 | 0, 1, 2 | 3 | Original world retained |

All 36 final compilations cover 5904 continuous phases. Every complete phase record exactly equals its frozen control, including internal events. Repeated full records are byte-identical. Proposal counts are 0, 1 and 2, and traces preserve exact prefixes. Both alternatives certify in these cases but fail the improvement requirement. **No new circuit quality improvement is claimed.** The observer covers source gates and public channels; this experiment does not claim complete observation of every physical route repeater.

The full 4096 two-input/three-output default family was rerun three times. Each entire record matches the previous immutable artifact exactly: the historical 27 improvements, 4069 holds and zero regressions are preserved. These runs use the fixed pre-clock library; its `selection.rs` exactly equals the final source. The final clock change affects only the separate fragment API.

Final checks:

- 15 API checks, including real counts/prefixes, stopping reasons, original refusal contracts and exact caller ports.
- 4 shared selection checks, including aggregate improvements concealing phase/channel regressions and altered certified views.
- 4 existing recursive consumer checks for empty/partial pins and pinned/unpinned packed roots.
- All 5 balanced-matrix integration checks, including actual full native seven-segment and ROM default selection; 318.68 seconds.
- Independent auditor: one real record and six corruptions; all corruptions refused.
- Native library and WASM library compile checks pass.
- Clippy exits zero with **131 warnings**. This is not a clean quality gate.

## Larger cases and remaining work

Both the frozen control and the pre-clock search library refuse the full native seven-segment recursive baseline: `d2` cannot supply `g29.in[1]` and the refused route has no legal refresh cell. The actual public default compiler still has its previously qualified matrix method; the fragment API remains a separate incomplete construction path. Raising its proposal budget cannot rescue a baseline refusal in this implementation. These refusal records are archived as negative evidence, not waveform acceptance.

The original ROM recursive control process remains live under session `17896`, last verified PID `1004549`. No completed ROM record or quality claim is included. The original experiment driver will continue the pre-clock ROM budget/repeat series after its control returns. It must be polled rather than restarted because an observation timeout expired.

Next work must resolve the larger recursive baseline construction failure, obtain broader composed candidates and complete primitive/event coverage. Larger ROM/Mux support, browser host clocks, sequential/HDL requirements, real Minecraft conformance and full-project gates remain incomplete. All eight completion-goal items remain active.

The adjacent JSON manifest binds source archives, actual libraries/executables, build arguments, dependency hashes, raw records, tests and exported worlds. Existing dirty frontend/planner/routing changes are retained in those archives as provenance, not adopted by this commit. The user's `examples/rom_family_physical_probe.rs` is untouched.
