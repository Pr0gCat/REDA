# Native public-output wiring qualification

The native physical API previously passed frontend `Buf` gates directly to the physical compiler, unlike the CLI, which lowers them first. A constant-only module exposed a second defect: eight declared outputs became two public positions when pinout construction used a signal-keyed map. Reading ordered occurrences fixed the map but still left shared physical terminals in the raw compiler path.

The API now uses ordinary lowering and retains the original source graph through a checked source-to-implementation rewrite. When hardware already has a certificate, the source proof is composed with it; physical observations remain those of the actual implementation. Pinout construction consumes declared output occurrences in order and checks count, signal identity and duplicate labels.

The selector reuses the existing certified direct-literal generator for occurrence-preserving boundaries. Its supported input range now includes zero through seven inputs, with the existing limit of one through sixteen outputs. Recognition checks the complete truth table for constants, direct inputs and inverted inputs, independently of source names. For combinational aliases outside specialized families, the compiler adds real two-NOR output buffers only where an occurrence would otherwise share a terminal or directly expose an input. It compiles that implementation through the existing physical compiler and certifies its relation to the unchanged source graph. Generated names avoid source collisions. These gates are actual hardware, not fabricated source observations. Existing physical checks and proof budgets still fail closed; this change does not claim transition-quality dominance over earlier generated circuits.

## Evidence

`baseline-test.rs` is the original two-width negative test. `baseline.log.gz` records the earlier `Buf` refusal. `probe.rs` and `probe.log` also expose the constant-only 8-to-2 terminal collapse. This baseline reused the source-bound ROM64 library whose native physical API matches the parent source; the complete baseline library is not described as a pristine parent checkout.

The final candidate contains only the five changed source/test files listed in `manifest.json`, over parent `fe86b82`. `source-binding.json` hashes all 257 bound files. Unrelated workspace modifications were excluded.

Seven tests pass across three targets. The output-contract test covers input widths 0–7, all 255 input states, eight distinct public terminals per module, and every actual implementation gate. It compares declared order and positions, source-proof identity, API export/reload pinout and world fingerprints, and native/auto CLI exports with Yosys and Python unavailable. A second output-contract test covers 17 constant aliases and an XOR alias across all four input states, with distinct terminals, actual implementation gates, API reload and CLI export agreement. `generic-negative-probe.log` records their earlier 17-to-1 and 2-to-1 terminal collapse against the first passing literal-only candidate. The other five tests cover ordinary seven-segment outputs and actual implementation gates, shared inout export/reload and CLI operation, and selected/forward-declared inout readers. Logs and artifact hashes are retained alongside this report.

Compilation and tests ran serially with a 4 GiB aggregate memory limit, no swap and one Cargo worker. Debug assertions and overflow checks were enabled in optimized test/dev profiles. Minecraft was off. This is simulator and certificate evidence; the already completed ROM64 game qualification was not rerun.

## Reproduction

On a clean checkout containing this change, run the three test targets through `tools/run_memory_bounded.sh`, with `CARGO_BUILD_JOBS=1`, optimized test/dev profiles, assertions and overflow checks enabled. The target names are `native_output_occurrence_pinout`, `native_physical_inout`, and `native_inout_selected_assignments`; run serially with `--test-threads=1`. The occurrence test itself creates the API and CLI export artifacts and disables external frontends for its CLI subprocesses.
