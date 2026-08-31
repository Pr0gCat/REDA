# Task 6 implementation report

Status: `DONE`

## Outcome

- Added a realised primitive-level timing DAG with stable typed node and arc identities.
- Delay ownership is exact: route arcs charge only route-owned repeaters, primitive arcs charge the source primitive, and output delivery is separated from the zero-delay declared-output boundary.
- Max-plus analysis records arrival, required tail, slack, critical delay, and a deterministic predecessor using the lowest stable arc ID on ties.
- Typed simulator observations now preserve multiple identities at one physical cell while sampling that cell once.
- Production `CompiledCircuit` values carry authoritative typed observation sites derived from the certified physical candidate.
- Typed transition summaries retain every tied-worst transition index and one deterministic witness per tied transition.

## Defects found during implementation

1. Real route branch paths include a primitive-owned source mouth which is absent from `route.cells`.
   - Initial graph derivation rejected that coordinate as unresolved.
   - Resolution: primitive-owned source delay belongs to the primitive arc; route delay charges only coordinates owned by the route.
2. A declared output initially combined physical delivery and boundary identity in one arc.
   - Resolution: added a non-observable `OutputLanding`; the route arc owns delivery delay and a separate zero-delay `OutputBinding` exposes the declared output.
3. Repeated coordinates in a route path could be charged more than once.
   - Resolution: route-delay derivation uses a stable set of charged route-owned coordinates.

## Verification evidence

- `cargo test --lib -- --nocapture`: 708 passed, 0 failed, 63 ignored.
- `cargo test --test delay_model_reconciliation -- --nocapture`: 17 passed, 0 failed, 1 ignored.
- Final focused reconciliation after the stable full-adder identity lookup change: 2 passed, 0 failed.
- `cargo test --test compile_end_to_end -- --nocapture`: 14 passed, 0 failed.
- Realised timing graph unit tests: 7 passed, 0 failed.
- Timing module unit tests: 13 passed, 0 failed.
- Observer unit tests: 5 passed, 0 failed.
- Timing integration tests: 3 passed, 0 failed.
- `cargo check --all-targets`: passed.
- `git diff --check`: clean.
- `cargo clippy --lib --tests -- -D warnings` reaches only two pre-existing unrelated warnings: simulator `clone_on_copy` and a planner test-only `field_reassign_with_default`; no Task 6 warning was reported.

## Reconciliation coverage

- Every concrete sink has exactly one route timing arc matching the physical route-owned repeaters.
- Every signal-carrying primitive landing has the exact simulator primitive delay.
- Every output has one zero-delay output-binding arc.
- The full-adder `g21` critical predecessor is the `g19` route with three repeaters, not the equal primitive-arrival `g20` route with zero repeaters.
- Certificate mutation, duplicate arc identity, cycles, unresolved nodes, and delayed primitives without observable identity are rejected.

## Modified files

- `src/compile/fragment_synth/identity.rs`
- `src/compile/fragment_synth/mod.rs`
- `src/compile/fragment_synth/timing_graph.rs` (new)
- `src/compile/mod.rs`
- `src/compile/planner.rs` (test-only mock fields)
- `src/redstone/simulator/mod.rs`
- `src/redstone/simulator/observer.rs`
- `src/timing/mod.rs`
- `tests/delay_model_reconciliation.rs`

## Review

- External Codex/Claude subagent quotas were exhausted and CodeRabbit CLI was unavailable, so the final review was performed by the controller.
- The controller review split output delivery from boundary identity, deduplicated route delay coordinates, changed the full-adder assertion to a stable `InstanceId` lookup, and reran focused reconciliation.
- The production front door remains legacy-compatible. Task 7 will seal timing derivation behind certified candidate promotion; Task 13 alone may switch production and remove the legacy path after acceptance.
