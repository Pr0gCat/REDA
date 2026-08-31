# Task 5 implementation report

Status: `DONE_WITH_CONCERNS`

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
  - 7 passed, 0 failed.
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
- Ring refusal tests passed without changing the existing `PhysicalInvariant` behavior.

## Authority extraction evidence

- Durable ownership moved to `compile::routing` for:
  - `PhysicalRouter`, `RouteRequest`, `RouterLimits`, typed endpoints/sinks, `NonEmptyRouteSinks`, typed physical reservations and failures.
  - `RealisedRouteTree`, branches, terminal records, placed blocks, delayed ownership, and terminal policy types (candidate/planner re-export them).
  - `route_step_is_legal`, exact full-state branch realisation, and StrengthAware state/dominance/reconstruction.
- Production `realise_branch_from` exists only in `routing.rs`.
- Production StrengthAware expansion exists only in `routing.rs`; planner's `strength_aware_astar` is a thin reservation/own-join/pricing adapter.
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
- Local review found and fixed a shared-trunk bug: a later fanout branch could reconstruct and overwrite an already-owned exact `BlockState`; existing state now wins and a regression test pins delay/facing preservation.
- Exact-file rustfmt used `skip_children=true`; no broad recursive formatting was run.

## Concerns

1. **Major: the extraction is not yet the single complete physical authority required by the brief.** Shipping DistanceOnly routing, `try_move`, `OwnJoinCheck`, `anchor_is_free_for`, `reserve_path`, legacy `Reservation`/`Occupancy`, `ring_closed_in`, and the body of `lay_net` remain in `planner.rs`. `LegacyPlannerRouterAdapter` passes through the typed API and preserves parity, but still invokes that legacy kernel. This is a typed seam plus partial authority extraction, not removal of both physics copies.
2. **Major: legacy limits are compatibility sentinels, not full work accounting.** The durable router accumulates queue/expansion counters across ordered sinks and deterministically rejects zero. The legacy adapter uses fixed `u64::MAX` values to preserve the Task-13 shipping front door and only enforces zero directly; it does not count every legacy A* insertion/expansion.
3. **Major: `try_move` still calls the legacy DistanceOnly search directly.** It has not been converted to issue one complete typed request through `LegacyPlannerRouterAdapter`.
4. The brief requests the shared local legality function in expanded verification, but `src/compile/verification.rs` is outside this task's write allow-list. Moving the check earlier into candidate validation changed a legacy refusal/certification category and was reverted. The durable router uses the shared rule; legacy emitted-world verification retains its existing equivalent axis logic.

These concerns are why this report is `DONE_WITH_CONCERNS`, not `DONE`.
