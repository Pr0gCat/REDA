# Ordinary output fanout isolation

Baseline: `726e681`. The native two-input ROM table `[1,3,2,0]` has a real output-lamp error after the repeated setup/target history reaches address 3 from address 0. Every gate driver is correct, but the exported `!b` lamp reads true instead of false. The independently recorded route/power evidence is in `generator-mixed-support-backfeed-2026-09-30.md`.

The exported signal also feeds a NOR support through a bare dust landing. Another input repeater strongly powers that support, which energizes the exported signal's dust backward. The existing pinned-output fanout rule prevents this with a diode at gate sinks, but ordinary declared output lamps were absent from its predicate.

The fix extends that existing routing requirement to all declared outputs before routing. It depends on graph connectivity, not ROM identity, module names, or table contents. Merge terminals already require diodes. Internal nets retain their existing policy. No post-routing block edits, metric-cap changes, or gate/block-count acceptance criteria are introduced.

## Validation

- `observed_output_isolation`: original native source, renamed input signals, and reversed input declaration order; all 16 setup/target pairs per variant (96 physical phases total). Every internal gate driver and all three output lamps match the independent expected function. Repeated compilation has an identical canonical world fingerprint. Exported fanout routes cannot use `DirectedDustIntoSupport`.
- `frontend_quality_report` and `generator_quality_matrix`: all existing caps pass, covering 1312 measured transitions across native and fresh Yosys circuits.
- `constant_drivers`: four physical tests pass. `dff_physical`: native and fresh Yosys physical test passes.
- `compile::routing::tests`: 67 tests pass. The pinned-output routing requirement unit test also passes.
- The updated `rom_output_history_diagnostic` passes all 32 physical history phases, including exact agreement between the route-ledger world and the emitted stock world. It now fails on incorrect lamps instead of expecting the historical defect.
- Six-case `torch_row_entry_records` shipping comparison: all nine metrics exactly equal the frozen `generator-torch-row-shipping-comparison-2026-09-30.json` control. The independently frozen control is unchanged.

One existing planner fixture, `a_pinned_output_that_also_feeds_a_gate_is_isolated_at_that_gate`, fails its prerequisite asserting that sibling `g1` lands as a repeater. Running that exact test against the unmodified baseline `726e681` reproduces the same failure. Its assertion is preserved; no test is ignored or weakened. This record does not claim the complete repository suite passes.

The baseline's wrong lamp makes its quality invalid, so no timing improvement percentage is inferred. This iteration fixes a concrete physical correctness defect while preserving the measured quality controls. Automatic stock/torch-row selection remains disabled pending broader qualification, and this does not prove that all internal mixed-support fanouts are safe. The optimization goal remains active.
