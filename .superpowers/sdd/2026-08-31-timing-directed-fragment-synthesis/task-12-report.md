# Task 12 replacement-gate report

The clean-checkout release replacement gate failed. No production shipping configuration was generated, so Task 13 is forbidden by its precondition.

## Clean-checkout commands

- `cargo test --release --test fragment_synth_acceptance -- --nocapture`: 4 passed after the measured report was generated.
- `cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output tests/fixtures/fragment_synth_shipping.json --shipping-source src/compile/fragment_synth/shipping_config.rs --shuffle-seed 0x5245444120260831`: completed with `replacement_gate_passed=false`, `shipping_evaluations=None`, six named failures.
- `src/compile/fragment_synth/shipping_config.rs`: absent before and after the run.

## Budget-zero measurements

- `and4`: certified, but 18 to 120 settle ticks and 472 to 1477 non-air blocks.
- `verilog:and4`: certified, but 22 to 238 settle ticks and 480 to 2121 non-air blocks.
- `full_adder`: independent seed routing refused route 0 as `PhysicalInvariant`.
- `segment_a`: route 4 exceeded the queue-entry cap.
- `seven_segment`: route 4 exceeded the queue-entry cap.
- `pinned:verilog:seven_segment`: route 0 exceeded the queue-entry cap.

Because correctness/seed acceptance failed at budget zero, higher optimisation budgets and cross-process repeatability were not executed. The JSON says `repeatability_executed: false` and records only planned seeded orders; it does not claim unperformed runs.

## Outcome

The explicit `compile_fragment_synth` API, timing graph, certified sparse seed, single-instance optimisation, and combinational duplication remain available for further work. Legacy production front doors remain unchanged and no legacy generator is deleted.
