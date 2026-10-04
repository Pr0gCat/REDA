# Scratch collection removes a measured raw-compile bottleneck

The raw ROM64 compiler materialised each gate and input in a fresh, circuit-sized scratch world, then scanned every cell to recover a handful of primitive blocks. On the clean c45bb2e source, two separately instrumented compilations completed in 208.54 and 198.23 seconds. The finer trace attributes 83.80 seconds to the certified adapter and 83.76 seconds to reconstructing the observations adapter.

An initial route-prefix indexing proposal left these stages at 80.09 and 80.22 seconds and was withdrawn. Its exact patch, tests and trace are retained as rejected development evidence; it is not part of the adopted source or current test count.

The adopted change uses each fresh scratch world's existing write ledger, sorts the flat indices into the original YZX order, reads the final state at each written position, and still excludes final air. All three callers were traced: each creates an all-air scratch world, writes one primitive/template, never clears the ledger, collects it once, then drops the world. This consumes no live simulation world's ledger. It changes neither World nor simulator nor physical-verifier code, ownership, checks, tolerances, routing settings, geometry or selection budgets.

The same ROM64 fixture completes raw compilation in 45.45 seconds, with adapter stages of 7.86 and 7.82 seconds. These are development observations under the same release profile and existing background jobs, not a controlled general speed benchmark. This does not measure the full default selector's retained-history qualification time.

The unchanged complete compile_end_to_end integration suite passes 14 tests, with zero failures, ignores or filtered tests. It checks primitive and typed ownership, exact candidate/world reconstruction, fanout and merge terminal identities, pinned caller behavior, truth tables, serialization, determinism and cycle rejection. The independently produced original control world and newly generated retained world were both reloaded by the existing standalone fingerprint helper and match exactly: 90ad27236b013529f500c04347b760a04e08851abd88a1ee9c657ae9eaafaa37. The original control's executable hash still matches its frozen source binding. This is semantic world equality, not an equal block-count criterion.

The original ROM64 process had already printed its saved artifact by 00:56; an empty final-result log does not mean raw generation is still running. Its continuous-waveform control remains live and has no new completed quality result. The six-input proposal remains isolated; production admission is unchanged. No broad gap is declared complete.

## Reproduction

Archive clean c45bb2e. For the original fine timing, apply fine-instrumentation.patch and copy baseline-probe.rs to examples/compile_stage.rs. For the qualified timing, additionally apply sparse-source.patch and use compile_stage.rs instead. Copy rom64_matrix.sv to tests/fixtures/. Build the example with cargo build --release --example compile_stage, then run it with the fixture path and top rom64. Use the same large-stack environment documented in the project experiments. Instrumentation is diagnostic only and is not staged into production. qualification.json binds the exact source patches, probe, fixture, binary hashes, tests and retained worlds.
