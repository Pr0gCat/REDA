# Preserve panic diagnostics during parallel certification tests

Two worker-driver tests intentionally catch panics, but their output helper changed the process-global panic hook. Restoring a saved hook does not make overlapping guards safe: guard A saves the original hook, guard B saves A's silent hook, A restores the original, and B finally restores silence. Other test failures then lose their diagnostics. The arithmetic qualification root run recorded seventeen failures without their panic messages.

The helper is removed. The existing test harness captures intentional panic output, while the same lowest-index error/panic and worker scheduling assertions remain. No tests are deleted or skipped; no compiler, simulator, verifier, timing or quality threshold changes.

A separate two-thread controlled experiment extracts the exact old helper from parent `bccd475` and fixes the overlapping guard order with channels. An unrelated caught panic produces zero reports with the old helper and one report with ordinary execution. All seven existing worker-driver tests pass after removal. The full worktree root run reports 1,167 passed, 17 failed and 71 ignored in 311.63 seconds, with the same seventeen failure names and all seventeen panic reports visible; this change does not claim to repair the seventeen existing circuit failures or complete the project goal. Other arithmetic/search changes in the worktree remain separately unqualified.

Raw reproducer sources and worker output are retained as compressed artifacts beside this document. The worker check is `RUST_MIN_STACK=134217728 cargo test --release --lib compile::fragment_synth::certification::driver_tests` after sourcing `tools/env.sh`.
