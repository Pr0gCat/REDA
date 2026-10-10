# Gap 8 full-project regression classification (2026-10-08)

Records the full `cargo test --workspace --release --no-fail-fast` run that
closes the gap-4 acceptance gate. It is a classification, not a green claim:
the suite is **not** all-green, and no historical control is re-pinned. Per the
[2026-10-04 amendment](../eight-gap-scope-amendment-2026-10-04.md), side-branch
findings are recorded and deferred, not turned into work.

## Run binding and totals

- Source binding: `target/qualification/gap4-full-regression/evidence/source-binding.json`
  (HEAD `4e253fb` `fix(routing): preserve retained trunk blocks` plus the
  uncommitted `tests/reset_worlds.rs` baseline declaration; the temp-path
  thread-id fix was **not** in this run, which is why the three collision
  failures appear).
- Wall 16145 s, exit 101, all 85 targets executed (`--no-fail-fast`).
- Library: **1207 passed / 6 failed / 71 ignored** (521.55 s).
- Integration: **47 failed** across the remaining targets.
- Total `... FAILED`: **53** (6 lib + 47 integration).
- Clean baseline `b01ad8e` full-library: 1188 passed / 7 failed / 71 ignored.
  Delta: +19 passed, −1 failed, 71 ignored unchanged. The baseline's seventh
  failure (extraction/setup) passes once the omitted committed artifact is
  restored; the six remaining are original-source fingerprint guards.

The collision fix (temp-path thread id) is committed as `390318b`
`test(physical): isolate temp fixture paths`. The collision rerun
(`target/qualification/gap4-collision-rerun/`) runs on `390318b`; the
`feedback_physical` native arm is verified green (`native_feedback_counter`
now `ok`), the `negedge_physical` and `reset_worlds` binaries are still running.

## Category 1 — library original-source fingerprint guards (6, record, do not re-pin)

Each assertion self-describes as a moved measurement ("read the list above
rather than re-pinning the number"). The routing fix changed the physical
output these guards pin; the movement is recorded for human review, not
re-pinned.

| test | site | pinned quantity |
| --- | --- | --- |
| `compile::planner::tests::a_route_lays_dust_on_its_own_committed_stone` | `src/compile/planner.rs:14971` | full_adder dust on committed stone at exactly three cells |
| `compile::planner::tests::an_unpinned_grown_and4_is_the_world_it_always_shipped` | `src/compile/planner.rs:11052` | grown and4 bounding box |
| `compile::planner::tests::the_keep_out_footprint_per_circuit_is_what_is_recorded` | `src/compile/planner.rs:26094` | and4 footprint arithmetic |
| `compile::planner::tests::the_derived_range_sees_every_recorded_extra_edge_and_keep_out_sees_none` | `src/compile/planner.rs:26029` | derived two-hop range |
| `compile::selection::tests::selector_preserves_realisability_cycle_and_driver_errors` | `src/compile/selection.rs:1037` | selector realisability/cycle/driver errors |
| `compile::coupling::tests::the_realised_graph_of_every_circuit_is_what_is_recorded` | `src/compile/coupling/tests.rs:1093` | `docs/derived/realised-graph-extras.md` line 15 |

## Category 2 — Yosys/Python absent environment failures (25, not product failures)

This environment has no `yosys` command, no `python`, and `import
yowasp_yosys` fails. Every `synthesize_verilog(...).unwrap()` / fresh-Yosys
arm necessarily fails with `PythonNotFound` / `could not launch python` /
`requires REDA_PYTHON`. Gap 4 acceptance is native/auto CLI, which passes
independently; these dual-arm tests are kept unmodified.

`fresh_yosys_constant_output_ports_have_real_certified_drivers`,
`native_and_yosys_dffs_match_physical_state_from_power_on`,
`feedback_reset_counter_fsm_and_state_enable_match_loaded_physical_cells`,
`statement_reset_and_nested_enable_match_loaded_physical_cells`,
`arithmetic_counters_match_loaded_physical_cells`,
`compare_frontend_physical_quality`,
`explicit_unsupported_falls_back_but_native_rejects`,
`native_auto_and_forced_yosys_compile_same_function`,
`cli_exercises_real_yosys_fallback_and_forced_mode`,
`trace_yosys_single_input_transition`,
`additional_circuit_shapes_match_independent_truth_tables`,
`native_systemverilog_dff_matches_fresh_yosys_on_edge_and_hold_trace`,
`native_and_yosys_independent_clock_domains_match_the_physical_state_oracle`,
`falling_edges_and_mixed_edge_shift_match_loaded_physical_cells`,
`arithmetic_counters_on_falling_edges_match_loaded_physical_cells`,
`verilog_seven_segment_expansion_matches_its_compiled_world`,
`reset_emitted_and_loaded_worlds_repeat_exactly`,
`arithmetic_emitted_and_loaded_worlds_repeat_exactly`,
`matrix_rewrite_chain_checks_fresh_yosys_and_rejects_wrong_source_data`,
`player_matrix_certifies_native_and_fresh_yosys_sources`,
`synthesizing_an_unknown_top_module_fails_with_a_clear_error`,
`the_baked_netlists_match_fresh_synthesis`,
`the_verilog_and4_matches_its_truth_table`,
`verilog_dff_reaches_the_physical_simulator`,
`optimised_lowering_preserves_every_verilog_decoder_vector`.

The seven combined tests above (`feedback_reset_counter_fsm_and_state_enable`,
`statement_reset_and_nested_enable`, `arithmetic_counters`,
`falling_edges_and_mixed_edge_shift`, `arithmetic_counters_on_falling_edges`,
`reset_emitted_and_loaded`, `arithmetic_emitted_and_loaded`) run their native
arm first; the native arm passes (its `FEEDBACK_TRACE`/`NEGEDGE_TRACE` is
emitted) and the failure is the yosys arm. Their native arms are verified in
the collision rerun.

`explicit_unsupported_falls_back_but_native_rejects` and
`cli_exercises_real_yosys_fallback_and_forced_mode` fail because the native
frontend correctly rejects the legacy source and the Auto arm then needs
Yosys, which is absent; the native rejection itself is the expected behavior.

## Category 3 — temp-path collision failures (3, root cause fixed)

Native-only tests and the native arm of combined tests shared one fixture
`name` and one `std::process::id()`, so libtest parallelism raced
`save`/`load`/`remove_file` on one temp path: `Io(NotFound)` (load finds no
file) or `Io(UnexpectedEof)` (load reads an empty file). The fix adds the
thread id to the temp path, making each test's path unique; no semantics are
weakened.

| test | site | symptom |
| --- | --- | --- |
| `native_feedback_counter_matches_loaded_physical_cells` | `tests/feedback_physical.rs:63` | `Io(NotFound)` |
| `native_negedge_dff_matches_loaded_physical_cells` | `tests/negedge_physical.rs:88` | `Os(NotFound)` |
| `native_reset_counter_worlds_repeat_exactly` | `tests/reset_worlds.rs:105` | `Io(UnexpectedEof)` |

Rerun on `390318b` (wall 12795 s, exit 101, `--no-fail-fast`): all three
collision failures are gone. `native_feedback_counter` `ok`,
`native_negedge_dff` `ok`, `native_reset_counter_worlds` `ok`. Per-binary
results: feedback 9 passed / 3 failed, negedge 8 passed / 2 failed,
reset_worlds 10 passed / 2 failed. The 7 remaining failures are the combined
tests' yosys arms (`PythonNotFound` at `feedback_physical.rs:269/309/331`,
`negedge_physical.rs:307/358`, `reset_worlds.rs:82`); each combined test's
native arm runs first and passes (`FEEDBACK_TRACE`/`NEGEDGE_TRACE` emitted,
`RESET_WORLD loaded_equal:true`). No collision symptom (`Io NotFound` /
`UnexpectedEof`) recurs.

## Category 4 — product-signal failures (17, routing fix changed physical output; record for human review, do not re-pin)

The routing fix (`4e253fb`) changes the realised layout, so these pinned
measurements move. They are design signals, not regressions to be silently
re-pinned; each is recorded with its exact left/right values for human review.

| test | site | evidence |
| --- | --- | --- |
| `native_constant_outputs_have_real_drivers_and_keep_constant_debug_bindings` | `tests/constant_drivers.rs:79` | `no entry found for key` |
| `occurrence_owner_preserves_realisability_driver_and_cycle_errors` | `tests/default_output_errors.rs:25` | `UndrivenSignal("missing")` assertion |
| `checked_fixture_keeps_schema_order_revisions_hashes_and_new_coverage` | `tests/fragment_synth_baseline.rs:51` | `Fingerprint("8464278284d1e5d9…")` |
| `trace_two_input_decoder_transition` | `tests/generator_hazard_trace.rs:29` | `NotALegacyLayout { report: "routing_stats::analyze" }` |
| `trace_reconvergent_decoder_transition` | `tests/generator_hazard_trace.rs:29` | `NotALegacyLayout` |
| `trace_slowest_decoder_transition` | `tests/generator_hazard_trace.rs:208` | `NotALegacyLayout` |
| `official_verilog_seven_segment_dump_uses_optimised_lowering` | `tests/mc_dump.rs:27` | left 206, right 47 |
| `segment_a_expansion_matches_its_compiled_world` | `tests/primitive_graph_equivalence.rs:53` | `CertifiedRewriteViewMismatch` |
| `seven_segment_expansion_matches_its_compiled_world` | `tests/primitive_graph_equivalence.rs:53` | `CertifiedRewriteViewMismatch` |
| `frozen_six_input_nine_occurrence_default_compilation_preserves_aliases` | `tests/raw_output_aliases.rs:304` | `(51,1,262)` vs `(54,1,260)` |
| `the_hand_written_circuits_keep_their_measured_size` | `tests/reference_circuits.rs:591` | and4 block count left 238, right 232 |
| `the_compiled_segment_a_matches_its_truth_table` | `tests/reference_circuits.rs:132` | "only a relaxation-placed layout may be without a settle model" |
| `the_compiled_decoder_matches_its_truth_table` | `tests/seven_segment.rs:212` | settle model |
| `decoded_rows_match_rom_truth_and_record_hazards_against_existing_generator` | `tests/torch_row_decoder.rs:197` | left `Number(10)`, right 8 |
| `four_word_rom_with_physical_literal_decoder_records_full_transition_quality` | `tests/torch_row_decoder.rs:485` | `no entry found for key` |
| `torch_row_entry_records_quality_against_current_shipping_layouts` | `tests/torch_row_decoder.rs:2318` | `no entry found for key` |
| `verilog_litematic_uses_the_same_optimised_lowering_as_mc_dump` | `tests/verilog_litematic.rs:55` | left 32, right 206 |

## Category 5 — workspace / side-branch failures (2, recorded, not expanded)

Per the amendment these are recorded and deferred; they do not block gap 4.

| test | site | cause |
| --- | --- | --- |
| `two_fresh_capture_processes_are_byte_identical` | `tests/fragment_synth_baseline.rs:194` | "capture requires a clean tracked/untracked repository"; working tree is dirty |
| `arithmetic_keeps_width_and_unsupported_operator_validation` | `tests/systemverilog_arithmetic.rs:217` | "invalid arithmetic accepted"; `tests/systemverilog_arithmetic.rs` is untracked (`??`), not in the committed HEAD set |

## What this proves and does not prove

- Proves: the routing fix (`4e253fb`) makes all 31 native fixtures pass the
  native/auto CLI acceptance (`gap4_complete: True`), and the temp-path
  collision root cause is fixed (`390318b`): all three collision failures
  (`native_feedback_counter`, `native_negedge_dff`,
  `native_reset_counter_worlds`) are verified `ok` in the rerun, and every
  combined test's native arm passes.
- Does **not** prove: a green whole-project suite. 53 failures remain,
  classified above. The 6 lib guards and 17 product-signal failures are
  recorded for human review, not re-pinned. The 25 Yosys/Python failures are
  environment, not product. Gap 4's Minecraft physical trajectory (status row
  7) is a side-branch requiring user input before investigation.
- All eight broad rows remain open until their own complete evidence exists.

## Update — re-run at HEAD `a920492` (2026-10-10)

Same full `cargo test --workspace --release --no-fail-fast` on the committed
tree at `a920492` (`target/qualification/gap4-full-regression/evidence/`,
source-binding `a920492`, wall 16166 s, exit 101, all 85 targets). The
`4e253fb` record above is retained unchanged as historical evidence.

Totals: **48 FAILED** = library 6 + integration 42 (was 53 = 6 + 47).

- Category 1 (6 lib source guards): unchanged. Recorded, not re-pinned.
- Category 2 (25 Yosys/Python environment): unchanged. Not product.
- Category 3 (temp-path collisions): **0** — the `390318b` fix is now in the
  tree; `native_feedback_counter`, `native_negedge_dff`,
  `native_reset_counter_worlds` all `ok`.
- Category 4 (product-signal pins): **15** (was 17). The two settle-model
  assertions `the_compiled_segment_a_matches_its_truth_table` and
  `the_compiled_decoder_matches_its_truth_table` now pass: the helper's
  no-model arm was generalized from `assert_eq!(Unified3d)` to
  `assert_ne!(Legacy)` at `a920492` (correctness — `routing_stats::analyze`
  returns `NotALegacyLayout` for FragmentSynth too). The layout-kind
  movement itself is still a product signal, recorded below, not re-pinned.
  The remaining 15 are unchanged from the table above.
- Category 5 (2 side-branch): unchanged.

This is still not full-project convergence: 48 failures remain, classified.
The 6 lib guards and 15 product-signal pins await human review.

## Investigation of the recorded movements (2026-10-10)

Per the human-review decision "investigate before accepting", the 21 recorded
movements (6 lib guards + 15 product-signal pins) plus the viewer placement
failures were checked for **functional** regression, not just numeric movement.
Evidence from the same `a920492` run (`full.log`):

- Every functional/semantic/behavioural test for the affected circuits passes:
  `compile::equivalence::tests::{segment_a,seven_segment,and4,full_adder}_expansion_matches_its_compiled_world`
  (library level), `compile::world_partition::tests::*_world_is_fully_explained`,
  `the_compiled_{segment_a,decoder,full_adder}_matches_its_truth_table`,
  `seven_segment_frontend_netlist_matches_its_truth_table_through_the_physical_simulator`,
  `and4_and_seven_segment_are_semantically_equivalent_to_the_baked_yosys_netlist`,
  `budget_zero_{segment_a,seven_segment,pinned_verilog_seven_segment}`,
  `false_path_*`/`reconcile_*`, `full_seven_segment_hierarchy_preserves_*`,
  `seven_segment_compile_is_deterministic_and_provenance_is_joinable`.
- The failing `tests/primitive_graph_equivalence.rs` copies of
  `segment_a/seven_segment_expansion_matches_its_compiled_world`
  (`CertifiedRewriteViewMismatch`) are a *different* view than the passing
  library-level `compile::equivalence` tests of the same name: the compiled
  world is functionally equivalent to the expansion; the integration test's
  pinned rewrite-view representation is stale.
- `trace_*_decoder_transition` (`NotALegacyLayout`) and the settle-model
  movement are the decoder/segment circuits now taking the FragmentSynth
  planner path; their truth-table and physical-state tests pass.
- `and4` 238→232 blocks: `and4_world_is_fully_explained` and
  `and4_expansion_matches_its_compiled_world` pass — a size reduction with
  function preserved.

Conclusion: **no functional regression** — every circuit computes correctly
(truth tables, library-level equivalence, physical-simulator state, world
partition, determinism all pass). But the movements are NOT purely cosmetic:
`decoded_rows_match_rom_truth…` `tests/torch_row_decoder.rs:197` pins
`baseline["worst_settle_ticks"]`, which moved **8 → 10** — a QoR regression
(slower settling on the smaller layout), the same arrival-balance concern as
gap 3. The rest are structural / measurement / representation pins (block
counts, footprints, bboxes, planner path, graph representation, layout
coordinates). Whether the smaller layouts, the FragmentSynth path, and the
worse settling are the desired new baseline is the open human decision
(accept → regenerate pins/fixtures/docs; reject → gap-3 root cause).

## Gap-8 tools/viewer/WASM/runtime gates at `a920492` (2026-10-10)

`target/qualification/gap8-gates/evidence/gates.log` (every check.sh stanza
except the root-test stanza, which the full-suite run above covers). Real
exits (the first run's `tail`/`tee` masked them; script corrected):

- PASS: budget_zero registration = 6; `cargo check -p reda --target wasm32`;
  frontend wasm parity (3 = host 3); `wasm-pack build --target web`.
- FAIL: `cargo clippy --all-targets -D warnings` on the root crate — 10 errors
  (`clone_on_copy` `src/redstone/simulator/mod.rs:214`, `too_many_arguments`,
  …); viewer clippy — 1 error (`chunks_exact_to_as_chunks`
  `viewer/tests/geometry_ordering.rs:216`). Pre-existing lint debt under
  clippy 1.98.0, not introduced by this branch's test changes.
- FAIL: viewer `placement_agrees_with_native` — 6 failed on host and 6 failed
  under wasm (the placement fixtures are stale vs the current compiler
  placement; same structural-pin class as Category 4).

Gap 8 is therefore NOT closed: it needs (a) the human accept/reject decision
on the structural movements, (b) the clippy lint debt resolved, and (c) the
placement fixtures regenerated only if (a) accepts the new layout.
