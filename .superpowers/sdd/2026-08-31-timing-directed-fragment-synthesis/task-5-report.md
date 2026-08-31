# Task 5 implementation report

Status: `DONE`

## RED evidence

1. Added `compile::routing` plus typed request/limit/axis/fanout tests before the API existed.
   - Command: `cargo test --lib compile::routing::tests -- --nocapture`
   - Result: failed to compile with 25 expected unresolved durable API symbols, including `RouteEndpoint`, `RouteRequest`, `RouterLimits`, `PhysicalRouter`, `PhysicalReservations`, `NonEmptyRouteSinks`, `RouterFailure`, and `route_step_is_legal`.
2. Added the fragment typed-identity adapter test before `FragmentRouterAdapter` existed.
   - Command: `cargo test --lib compile::routing::tests -- --nocapture`
   - Result: failed to compile only on missing `FragmentRouterAdapter`.
3. During GREEN, `delay_model_reconciliation` exposed an unintended legacy certification-order change when local repeater-axis checking was inserted into candidate timing validation.
   - Failure: `structural mismatch ConnectionTopology at Route(RouteId(40))` on the decoder fixture.
   - Root cause: the local check changed legacy certification/classification ordering even though the typed route round-trip was byte-equivalent.
   - Resolution: keep local axis authority in the durable router/search/certification path and leave emitted-world legacy topology/dust checks in their established order. The same test then passed all six fixtures.

## GREEN evidence

- `cargo test --lib compile::routing::tests -- --nocapture`
  - 8 passed, 0 failed.
  - Covers non-empty ordered typed sinks, request-scoped zero limits, full-state fanout, distinct fragment sink IDs, exact repeater rear/front axis, `WrongRepeaterAxis { ConnectionId, Anchor }`, non-default delay preservation, and immutable shared-trunk state across later branches.
- `cargo test --lib compile::fragment_synth::realise::tests -- --nocapture`
  - 14 passed, 0 failed.
  - Includes non-default repeater delay 4 surviving candidate storage, emission, and physical verification.
- `cargo test --test delay_model_reconciliation terminal_repeaters_are_every_repeater_on_the_path -- --exact --nocapture`
  - 1 passed, 0 failed.
  - and4 10/10, full_adder 32/32, segment_a 83/83, seven_segment 156/156, verilog:and4 12/12, verilog:seven_segment 80/80 edges reconcile every repeater.
- `cargo test --test compile_end_to_end -- --nocapture`
  - 14 passed, 0 failed.
- Existing StrengthAware regressions:
  - `cargo test --lib the_strength_aware_search -- --nocapture`
  - 3 passed, 0 failed.
- Refusal/ring regressions:
  - `a_ring_through_a_repeater_is_found_and_a_sealed_lid_breaks_it`: passed.
  - `a_ring_through_a_repeaters_own_riser_is_found`: passed.
  - `lay_net_refuses_the_branch_that_closes_a_ring`: passed.

## Parity and refusal evidence

- The legacy front door now constructs typed endpoint/sink IDs, a typed reservation snapshot, and a `RouteRequest`, calls `LegacyPlannerRouterAdapter` through `PhysicalRouter`, then converts the exact typed tree back to the compatibility `Route`.
- Diagnostic field-by-field comparison during implementation confirmed anchors, full cell states, floors, terminals, and branch paths were unchanged across legacy -> typed -> legacy conversion.
- `compile_end_to_end` passed both `legacy_and4_extracts_to_a_legal_candidate_with_unit_seed_score` and `extracted_fanout_terminal_metadata_keeps_each_sink_identity`, plus exact legacy world re-emission.
- A permanent all-pinned full-adder differential runs the complete frozen rip-up loop and compares every legacy route against the typed adapter byte for byte. It caught an ownership-only socket preclaim being misread as exact dust; the fix preserves strict typed exact-state semantics without changing legacy output.
- Ring refusal tests passed without changing the existing `PhysicalInvariant` behavior.
- Complete current-source library regression: `cargo test --lib -- --nocapture` passed 695 tests with 0 failed and 63 ignored (758 total).
- Full delay reconciliation: `cargo test --test delay_model_reconciliation -- --nocapture` passed 15 tests with 0 failed and 1 ignored.
- `cargo clippy --lib` reports no Task-5 warning; the only warning is the pre-existing simulator `clone_on_copy` at `src/redstone/simulator/mod.rs:177`.

## Authority extraction evidence

- Durable ownership moved to `compile::routing` for:
  - `PhysicalRouter`, `RouteRequest`, `RouterLimits`, typed endpoints/sinks, `NonEmptyRouteSinks`, typed physical reservations and failures.
  - `RealisedRouteTree`, branches, terminal records, placed blocks, delayed ownership, and terminal policy types (candidate/planner re-export them).
  - `route_step_is_legal`, exact full-state branch realisation, and StrengthAware state/dominance/reconstruction.
- Production branch realisation exists only in `routing.rs`; the public `realise_branch_from` compatibility wrapper and frozen legacy reference are test-only.
- Production StrengthAware expansion exists only in `routing.rs`; planner's `strength_aware_astar` is a thin reservation/own-join/pricing adapter.
- Production DistanceOnly expansion, exact-state insertion, ring closure, request work accounting and local path certification are all owned by `routing.rs`. `planner::lay_net` and `try_move` issue typed requests instead of running another search/realisation kernel.
- Dust stair/support proof remains world-dependent and is not represented as complete proof by the local four-argument legality function.

## Modified files

- `src/compile/routing.rs` (new)
- `src/compile/mod.rs`
- `src/compile/fragment_synth/candidate.rs`
- `src/compile/fragment_synth/realise.rs`
- `src/compile/planner.rs`
- `.superpowers/sdd/2026-08-31-timing-directed-fragment-synthesis/task-5-report.md`

No production/test files outside the brief allow-list were modified.

## Self-review

- CodeRabbit CLI was unavailable (`coderabbit` not found), so no external CodeRabbit result exists.
- Two Claude read-only review rounds found search-time exact-repeater legality, ring charge attribution and later-branch state overwrite defects; each was fixed and regression-tested. Claude and Codex subagent quotas were exhausted for the final pass, so the last review was a controller review backed by the complete regression run above.
- Local review found and fixed a shared-trunk bug: a later fanout branch could reconstruct and overwrite an already-owned exact `BlockState`; existing state now wins and the production insertion helper is directly regression-tested.
- `git diff --check` is clean. A mistaken repository-wide `cargo fmt` invocation was reverted outside the three Task-5 work files; no unrelated file remains modified.

## Closed review concerns

1. Shipping DistanceOnly routing and `try_move` now use the typed routing authority. Frozen `legacy_lay_net_reference` and old search adapters compile only for differential tests.
2. Queue and expansion accounting is request-scoped and accumulates across ordered sinks. The shipping compatibility caller deliberately supplies `u64::MAX`; Task 13, not Task 5, owns any production cap policy change.
3. Expanded strict verification calls the same `route_step_is_legal` authority as strict routing. Legacy emitted-world verification retains its established classification order so extraction remains byte- and refusal-compatible.
4. Strict fragment routing forbids terminal transit, certifies exact repeater rear/front traversal during search and after realisation, and cannot replace an existing non-prefix trunk state. Legacy compatibility behavior remains isolated behind `route_with_policy` until Task 13 acceptance.
