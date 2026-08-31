# Task 8 implementation report

Status: `DONE`

## Outcome

- Added the independent `SeedInput`, sealed `SeedServices`, `SparseSeedBuilder`, and `compile_sparse_seed_with_services` seam. Its constructor contains only the selected library, typed router, durable emitter/verifier facades, complete certifier, and fixed search configuration.
- Built a complete one-to-one `InstanceGraph` before physical work and rejected stateful topology, malformed provenance, invalid pins, and zero placement bounds before calling a physical service.
- Added deterministic topology-layer placement with real expanding horizontal Manhattan-shell trials. Rejected footprint choices consume the fixed seed-backtrack cap; shell or backtrack exhaustion reports the stable instance, primitive, and radius. Unpinned IO is placed near its first consumer or exact driver; pinned input/output heights constrain the layer interpolation and caller-owned cells stay empty.
- Placed primitive variants and junction contributors explicitly. Torch supports expose distinct NESW input sockets, isolated merge branches are physical repeater primitives, and bare merge branches land directly on the junction.
- Scheduled fanout route trees by the earliest topological sink, not by source identity. Every external and internal `ConnectionId` is a typed routing obligation and every returned branch receives a `ConnectionBinding`.
- Protected all future source/sink cells before routing and added one-cell inter-net halos after routing while preserving every typed terminal. This prevents earlier nets from consuming later endpoints and prevents same-plane dust shorts.
- Added an exact route-owned normalising repeater at every dust-junction output. The repeater makes the route's strength-15 contract physical and assigns its one-tick cost to the route/timing ledger instead of assuming a bare merge refreshes strength.
- Emission, authoritative physical verification, and complete Task-7 certification all run before the seam returns `CertifiedCandidate`.

## Independent-path evidence

- The and4 call-spy regression reports positive router, emitter, verifier, and certifier calls.
- The production seam has no legacy type or service slot. Its counting legacy oracle and all five legacy entrypoint counters remain zero during independent generation.
- The explicit differential adapter is a separate test-only seam. Calling it once increments exactly the selected legacy oracle entrypoint once, proving the zero-call spy is wired.
- The production portion of `seed.rs` contains no call to legacy generation, relaxation, springs, or the old optimiser. The only `compile_legacy` call is inside the test-only differential oracle used to prove the zero-call production assertion is wired.

## Deterministic fixtures

- NOT.
- and4.
- fanout.
- two-primitive BUF.
- bare merge.
- mixed bare/isolated merge.
- fully isolated merge.
- pinned and4 with caller-owned input/output cells.

Each fixture builds twice through complete certification and requires identical candidate and emitted-world fingerprints.

## Verification evidence

- Final Task 8 seed suite: 10 passed, 0 failed in 193.83s. It includes the early-pin and bounded-placement regressions plus two complete deterministic certifications for every fixture.
- Final affected-case reruns:
  - fanout/BUF/bare-mixed-isolated merge fixture set: passed after internal-terminal protection.
  - pinned and4 double certification: passed after review fixes (181.88s).
  - invalid pin before physical services: passed.
  - deterministic Manhattan-shell fallback and exact backtrack-cap refusal: 2 passed.
  - junction endpoint promotion: passed.
  - Task 7 certification suite: 6 passed.
- Pin integration regressions: `build_circuit_pins` 4 passed; `terminal_handover` 27 passed.
- `cargo check --all-targets`: passed.
- `git diff --check`: clean.
- `cargo clippy --lib --tests`: no Task 8 warning; only the pre-existing simulator `clone_on_copy` and planner test `field_reassign_with_default` warnings remain.

## Review findings fixed

1. Source-ID route order let a late primary-input net cross the whole board before early instance outputs. Routes are now scheduled by earliest topological sink while retaining one fanout tree per source.
2. A bare junction was initially treated as strength 15. Junction outputs now begin with an exact route-owned normalising repeater and forced straight exit.
3. A route could approach a junction through an undeclared free face. Unused junction neighbours are reserved before routing.
4. Same-height pinned layouts allowed physically adjacent foreign dust routes. Completed routes now reserve a one-cell halo while all future typed endpoints remain protected.
5. Internal primitive terminals were computed during routing but not pre-reserved. Every pending external, internal, and output terminal is now protected before the first route.
6. Exact repeater counts were recomputed by scanning every route cell for every path cell. A route-local repeater set now makes the calculation linearithmic and deterministic.
7. Pin semantics were first rejected only after placement. Netlist-only pin validation now runs before any physical service.
8. The initial sparse origin mixed pin X and Z into one scalar and needlessly lengthened X routes. Only the X extent now shifts the X origin.

## Modified files

- `src/compile/fragment_synth/seed.rs` (new)
- `src/compile/fragment_synth/services.rs` (new)
- `src/compile/fragment_synth/certification.rs`
- `src/compile/fragment_synth/legacy_adapter.rs`
- `src/compile/fragment_synth/mod.rs`
- `src/compile/routing.rs`

## Review availability

- CodeRabbit CLI is not installed on this host, and Claude/Codex subagent quotas remain unavailable. The final review was a controller correctness review backed by focused certification and pin regressions.
- Production generation is still unchanged. Task 9 may consume this crate-private seam; Task 13 alone may switch production and remove legacy generation after acceptance.
