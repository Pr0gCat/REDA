# Native arithmetic and DFF feedback qualification

Base commit: `b8eca37`. Both four-bit synchronous reset/enable counters are qualified using the native frontend and emitted, exported and reloaded physical worlds. No Minecraft server execution is claimed.

## Changes

The native parser, typed RTL and lazy carry-prefix bit lowering support unary positive/negation and binary addition/subtraction. Cached carry prefixes preserve DFF identity when feedback requests register bits in arbitrary order. Overflow follows the existing fixed-width expression rules.

The physical API uses the existing optimized CLI lowering for stateful graphs and exposes their actual implementation graph. It does not attach a combinational Boolean certificate to DFF graphs. Combinational lowering and certificates retain their existing path.

Sequential compilation falls back from the fixed plan to existing bounded growth. Clock initialization now sets both raw low and inverted high clock cones and Design H clock controls to their evaluated zero-state levels, preventing power-on capture. Data/Q initialization, routing budgets, physics and assertions are unchanged.

## Evidence and acceptance

Increment: the same saved world passes 44 phases in `native-arithmetic-setup-replay/setup.log.gz`. Decrement: actual generation and qualification passes 44 phases in `native-arithmetic-candidate-v7/native_decrement_generates_correct_loaded_feedback.log.gz` (218.11 seconds). Each covers all 16 states, wraparound, enable hold and reset priority, every implementation gate, hidden DFF data/lock/clock cells, repeater locks and stored-data transition cardinality. Export/reload pinout and world fingerprints pass. Increment replay rechecks source, implementation graph, world fingerprint and pinout before execution.

The original no-setup protocol changes enable with the rising edge and fails phase35 (expected exit101). Two low-clock setup phases resolve this on the identical world; no assertion or settling ceiling is weakened. Both positive and negative results and replay source are retained.

Candidate v8 changes only the test file relative to executed production candidate v7. Both tiny clock regressions pass (raw/inverted edge and independent clock storage, 0.01 seconds each). Native/auto selection passes with empty PATH and a nonexistent Python executable (0.10 seconds): both counters select Native without fallback and retain identical source graph, output bindings and optimized implementation. Existing output occurrence/pinout tests pass (2 tests, 1.28 seconds).

Counter hardware is generated once per distinct optimized graph through the shared physical compiler. Native/auto public selector and common backend are proven separately; counter `build_circuit` CLI processes were not independently executed. Existing output-contract regression does execute the CLI for its combinational cases. This avoids regenerating identical counter hardware.

Three source arithmetic tests pass: integer oracle, precedence/reordered bit requests and counter state/edge behavior. One pre-existing untracked test expects implicit width mismatch rejection, while shared integral-context rules permit widening. The full old suite fails this assertion; this typing-policy conflict is deferred, and no assertion is changed. Equal-width counter fixtures do not depend on it. The unused BinaryOp::as_str warning remains.

## Reproduction and scope

`tests/native_arithmetic_generation.rs` contains executable counter and clock protocols; fixtures are in `tests/fixtures/arithmetic_counter_{add,subtract}.sv`. Candidate scripts and exact SHA256 source bindings accompany terminal logs. The increment world contains replayed phase data; decrement contains producer phase data. The artifact manifest hashes every retained file.

All heavy work is serialized using `tools/run_memory_bounded.sh`, 4 GiB aggregate memory, zero swap and one Cargo worker, with Minecraft off. Only the eight bound production files differ from base source. Unrelated dirty changes and `examples/rom_family_physical_probe.rs` are preserved. ROM64 and unrelated routing work were not rerun. Commit only these qualified changes; no push.
