# Native independent clock domains

The source elaborator imposed one remembered clock across all registers and instances. The responsible lower layers already retain a clock pin for every DFF: logic lowering reserves each register separately, the evaluator remembers each register's previous clock, and the physical Design H cell is wired to that register's clock net. Native elaboration now preserves those per-register aliases instead of rejecting the second scalar clock. There is no new gate kind, clock service, simulator rule, or physical initialization.

Supported forms remain positive-edge nonblocking plain and enable-style assignments. Each event control still has exactly one clock, directly supplied through a scalar top-level input; nested instance clocks still require direct scalar input aliases. Different blocks and instances can use different clocks. Multiple event edges, asynchronous/synchronous reset, negative edges, derived/gated clocks, and general self-feedback FSM/counter forms remain unfinished. This is real independent-clock support, not acceptance of formerly rejected syntax without physical generation.

The flat, hierarchical and cross-domain shift fixtures run through native and actual optional Yosys synthesis. Each of the six physical circuits is lowered, compiled twice, and compared including palette states and ports, then serialized to litematic and loaded before simulation. Power-on Q is checked before the first edge. All 16 prior/data combinations run through 15 phases each, including separate and simultaneous rising edges, independent falling edges, and data changes with both clocks high. All 240 phases retain history. The cross-domain shift also checks that simultaneous rising edges make the receiving register sample the previous source-register state; its expected behavior comes from the literal fixture, not from the compiled graph. Every source register's public driver and lamp is checked against an independent two-register capture/hold oracle and the frontend evaluator. The existing Design H protocol's 16-tick clock-torch dwell is unchanged; no extra stages, reset pulses, simulator initialization, or longer settling ceiling are introduced.

Complete event records for all six circuits were generated three times with identical JSONL bytes. The standalone Python auditor independently verifies clock pins, the complete stimulus protocol, state captures and retained driver/lamp event replay. Its negative check rejects wrong clock pins, wrong state, missing events and changed protocol. The earlier multi-instance rejection assertion is now a positive distinct-clock-pin test; all actual invalid connection/driver, recursion, derived-clock, multiple-event-edge and unsupported reset checks remain.

Validation: 28 release regression tests (12 source DFF,8 instances,2 existing physical DFF,6 frontend selection), the new physical domain trace test, the expanded missing-Python CLI test, independent audit/mutation check, and WASM library check pass. Native and Auto CLI generate both fixtures with `REDA_PYTHON=/definitely/missing/python` and report native selection. Root remains1164 passed/17 failed/71 ignored (exit101), with the same preserved failure names as parent `0e5ae42`; no full-project-green claim. Existing combinational generation and quality selection are not edited.

Reproduce the physical traces and audit:

```bash
source tools/env.sh
RUST_MIN_STACK=134217728 cargo test --release --test multiclock_physical -- --nocapture
python3 tools/compare_multiclock_probe.py docs/native-multiclock-2026-10-02-trace.jsonl.gz
python3 -m unittest discover -s tools -p test_compare_multiclock_probe.py
```

[Summary and hashes](native-multiclock-2026-10-02.json) accompany the compressed complete traces and verification logs. These are simulator and serialized-artifact proofs. Real Minecraft validation remains outstanding. The eight-part goal stays active; reset, negedge, general feedback, wider generator methods, general synthesis budgets, inout and root convergence still require work.
