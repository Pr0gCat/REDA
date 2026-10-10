# Gap 6 native completion check (2026-10-09)

Gap 6 requires every newly required and existing supported feature to
compile, execute and verify natively through its actual public entry with
Yosys absent. Independent Yosys comparisons remain optional; native subset
success does not establish the whole requirement
([status row 6](status.md)). This records the native coverage matrix and the
full-suite run that measures it. It is a classification, not a green claim.

## Run binding

- HEAD `5fe7adf` `test(native): verify verilog frontend features without
  Yosys` on the committed working tree.
- Environment: no `yosys` command, no `python`, `import yowasp_yosys` fails,
  `REDA_PYTHON` unset. Every `synthesize_verilog(...)` arm necessarily fails
  with `PythonNotFound` / `could not launch python`.
- Full suite: `cargo test --workspace --release --no-fail-fast --
  --test-threads=16` through `tools/run_memory_bounded.sh` (4 GiB cgroup,
  zero swap, `CARGO_BUILD_JOBS=1`). Measured totals recorded below.

## Native coverage matrix

Every file that calls `synthesize_verilog` has a native arm that runs through
`compile_systemverilog` (the native public entry) and passes with Python
absent. The yosys arm is the environment failure, not a product failure.

| file | native arm (passes natively) | yosys arm (environment) |
| --- | --- | --- |
| `constant_drivers.rs` | `native_constant_outputs_have_real_drivers_and_keep_constant_debug_bindings`, `native_constant_register_data_survives_clock_edges` | `fresh_yosys_constant_output_ports_have_real_certified_drivers` |
| `dff_physical.rs` | native arm of `native_and_yosys_dffs_match_physical_state_from_power_on` runs first | yosys arm of same test |
| `feedback_physical.rs` | native arm of the three combined tests runs first; `native_arithmetic_increment/decrement`, `native_feedback_counter/fsm/state_enable/reset_counter` | yosys arms of the three combined tests |
| `frontend_quality_report.rs` | native arm runs first | fresh-Yosys oracle row |
| `generator_hazard_trace.rs` | `trace_decoder_transition`, `trace_slowest_decoder_transition`, `trace_single_input_transition` | `trace_yosys_single_input_transition` |
| `generator_quality_matrix.rs` | native row of the native/yosys loop | yosys row of the loop |
| `m3_m4_dff_yosys_differential.rs` | native assertions of `native_systemverilog_dff_matches_fresh_yosys_on_edge_and_hold_trace` run first | yosys assertions of same test |
| `multiclock_physical.rs` | native arm runs first | yosys arm of same test |
| `negedge_physical.rs` | native arm runs first; `native_negedge_dff` | yosys arms of the combined tests |
| `primitive_graph_equivalence.rs` | `and4/full_adder/segment_a/seven_segment_expansion` (hand-authored netlists, no frontend) | `verilog_seven_segment_expansion` |
| `reset_worlds.rs` | native arm of the combined test; `native_reset_counter_worlds_repeat_exactly` | yosys arm of the combined test |
| `torch_row_decoder.rs` | `matrix_frontend_source` native arm; `player_matrix_certifies_native_and_fresh_yosys_sources` native arm first | `matrix_rewrite_chain_checks_fresh_yosys` |
| `verilog_frontend.rs` | the five native variants added in `5fe7adf` | the five original Yosys tests |

`verilog_frontend.rs` was the only file whose Yosys tests had no native arm
in the same file. The five native variants added in `5fe7adf` close that gap:

- `native_verilog_and4_matches_its_truth_table` — lowers `and4.v` natively,
  checks the 16-vector truth table.
- `native_verilog_dff_reaches_the_physical_simulator` — lowers `dff.v`
  natively, runs the documented seven-step state trace through the simulator.
- `native_optimised_lowering_preserves_every_verilog_decoder_vector` — lowers
  `seven_segment.v` natively, checks all 112 public values and the Pareto
  baseline `(56, 12348, 88)`.
- `native_baked_netlists_match_semantic_equivalence` — evaluates the native
  netlist and the baked `verilog::CIRCUITS` netlist over every input
  combination and compares port output values (semantic, not byte-for-byte:
  native lowering emits `gN` gate names, baked emits Yosys names).
- `native_synthesizing_an_unknown_top_module_fails_with_a_clear_error` —
  asserts the native error names the missing module.

The no-settle-model arm of the timing helper is duplicated in three test
files (`verilog_frontend.rs`, `reference_circuits.rs`, `seven_segment.rs`);
each copy asserted `PlannerKind::Unified3d`. `routing_stats::analyze` returns
`NotALegacyLayout` for a FragmentSynth layout too
(`src/compile/routing_stats.rs:917-923`), so the assertion ("only a
relaxation-placed layout may be without a settle model") was semantically
incomplete: the routing fix made these circuits ship FragmentSynth, and the
native tests `the_compiled_segment_a_matches_its_truth_table` and
`the_compiled_decoder_matches_its_truth_table` failed on the assertion, not
on their truth tables. All three copies are generalized to
`assert_ne!(Legacy)`: an emitter-laid-out layout must have a model; a
relaxation- or fragment-placed layout may be without one. This is
correctness, not a re-pin; the layout-kind movement itself stays recorded in
the full-regression classification for human review.
A second correctness fix, not a re-pin:
- `native_baked_netlists_match_semantic_equivalence` compares native and baked
  input name sets, not order: native lowers in declaration order (`d3..d0`),
  baked in sorted order (`d0..d3`), and the truth-table evaluation maps by
  name, so order is irrelevant.

## Measured native PASS / yosys environment failures

Full `cargo test --workspace --release --no-fail-fast` on HEAD `a920492`
(`target/qualification/gap4-full-regression/evidence/`, wall 16166 s, exit
101, all 85 targets): **48 FAILED** = library 6 + integration 42.

- Library 6: the original-source fingerprint guards (gap-8 Category 1),
  unchanged; recorded, not re-pinned.
- Integration 42: 25 Yosys/Python-absent environment failures (every
  `synthesize_verilog`/fresh-Yosys arm; `PythonNotFound`), 15 product-signal
  measurement pins (gap-8 Category 4), 2 dirty/untracked side-branch
  (`two_fresh_capture_processes`, untracked `systemverilog_arithmetic`).
- Every feature with a Yosys arm has a native arm that runs and passes: the
  seven combined tests emit their native trace first (`FEEDBACK_TRACE`/
  `NEGEDGE_TRACE`/`RESET_WORLD loaded_equal:true`) and only the Yosys arm
  fails; the five `verilog_frontend.rs` Yosys-only tests have passing native
  variants (`verilog_frontend` target: 5 native passed / 5 yosys
  `PythonNotFound`).
- Delta vs the `4e253fb` run (53 FAILED): −3 temp-path collisions (fixed at
  `390318b`, verified `ok`) and −2 settle-model assertions (fixed at
  `a920492`); `the_compiled_segment_a_matches_its_truth_table` and
  `the_compiled_decoder_matches_its_truth_table` are no longer in the failure
  list.

## What this proves and does not prove

- Proves: every feature that has a Yosys arm also has a native arm that
  compiles, executes and verifies through its actual public entry
  (`compile_systemverilog` + `compile` + `Simulator`) with Python absent. The
  five `verilog_frontend.rs` Yosys-only tests now have native variants.
- Does **not** prove: a green whole-project suite, nor the whole gap-6
  requirement. Independent Yosys comparisons remain optional and are not
  established here. Native subset success does not establish the whole
  requirement. The six library original-source fingerprint guards and the
  product-signal movements are recorded for human review, not re-pinned.
- All eight broad rows remain open until their own complete evidence exists.
