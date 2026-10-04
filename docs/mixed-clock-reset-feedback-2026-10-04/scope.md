# Mixed-clock synchronous reset and feedback trajectory qualification

The native source frontend and existing physical sequential backend already support this composed case. No backend change is necessary. The new source is a posedge domain with synchronous reset and enabled toggle feedback, followed by a separate negedge domain with its own synchronous reset and cross-domain state capture. This extends physical qualification beyond the existing plain multiclock and single-clock reset fixtures; it does not complete the broad sequential requirement.

Independent literal source oracle updates both domains from the same previous-state snapshot. Three separately executed fresh processes retain 264 continuous phases each (792 total), covering every pair of reset settings and both enable settings, separate and simultaneous opposite edges, and control changes with clocks held. The same existing 2000-tick stabilization cap and 16-tick Design H dwell are retained.

All three results pass. Every lowered gate, both public lamps, all ten master/slave cell sites, and repeater locking states match their independent final-state oracle. All 27 observed sites and 1999 events per run are replayed without dropped or duplicate events. Public lamps and state drivers have no extra transitions. Worst complete phase is 50 ticks, including the existing 16-tick dwell. This is a bounded, representative trajectory result, not general delay/quality dominance over an independent historical circuit.

Serialized worlds reload exactly, comparing every block. All three world fingerprints and parsed traces are identical. The independent Python auditor recomputes the literal state transitions, every NOR Boolean value and cell lock/value contract, then replays every event. Corrupted source state, corrupted cell state, missing event and duplicate event controls are rejected.

The qualified library source is an isolated clean archive of aef704769f52b21bd9996771f67517d7be058025 plus the two new test files. It excludes preexisting user-owned dirty compiler/frontend/test changes. Debug compilation and all three executions use `REDA_PYTHON=/nonexistent/reda-python`; no Yosys path is invoked. Sources, executable and decompressed logs are SHA256-bound in manifest.json. A separate current-worktree integration test also passes with the nonexistent interpreter. Its independent audit, exact world fingerprint and full parsed trace equal the clean HEAD qualification; the current mixed source tree is bound in current-worktree-manifest.json. This is not a whole-project result.

Reproduce:

```bash
REDA_PYTHON=/nonexistent/reda-python cargo test --locked --offline --test mixed_clock_reset_feedback -- --nocapture > mixed.log
python3 tools/audit_mixed_clock_reset_feedback.py mixed.log
```

Broader reset/FSM/counter trajectories, routing failures, historical continuous-quality comparisons, and real Minecraft conformance remain open. No simulator or physical rules, routing limits, original tests, goldens or ignore attributes changed.
