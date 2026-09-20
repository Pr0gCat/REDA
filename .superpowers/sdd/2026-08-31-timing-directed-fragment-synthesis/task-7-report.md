# Task 7 implementation report

Status: `DONE`

## Outcome

- Added one immutable transition-manifest policy: exhaustive ordered distinct pairs for zero through four inputs, and stable one-bit toggles among zero/one/one-hot/one-cold vectors above four inputs.
- Added fixed `SearchConfig::checked_defaults()` caps and a separately fingerprinted `CertificationConfig`; no cap is derived from wall time or synthesis budget.
- Added an all-input compositional equivalence proof. It independently instantiates every selected implementation, symbolically composes primitive Torch/Repeater/Junction semantics, validates duplicate and sink assignments, and emits a canonical certificate without enumerating `2^n` rows.
- Added `ExpandedCandidateCertifier`, the only promotion path to `CertifiedCandidate`: structural and physical verification, timing-graph derivation, equivalence, exhaustive truth through eight inputs, complete fixed-manifest sweep, and metric capture must all succeed.
- Every manifest entry uses a fresh simulator and cloned certified world. Source inputs are applied as a batch and settled, `start_tick` is recorded, destination inputs are applied as a batch, and quiescence is measured.
- Added request-scoped bounded simulator settling. The event cap is enforced before the next due event is applied and is shared by source plus destination work for one transition.
- Candidate quality is lexicographic in observed settle, non-air blocks, occupied volume, and static routed delay. Candidate fingerprint only stabilises complete ties and is never a quality improvement.

## Named refusals covered

- Stateful topology and combinational cycles.
- Missing, duplicate, wrong-signal, wrong-driver, and duplicate-input assignments.
- Unknown or mismatched selected implementation topology.
- A registered implementation whose primitive semantics do not implement its claimed logical gate.
- Equivalence proof, manifest transition, simulator event, and game-tick exhaustion.
- Functional output mismatch and transition divergence.
- Candidate/certificate timing fingerprint mismatch remains enforced by the Task 6 timing graph.

## Verification evidence

- Complete current-source library regression: 724 passed, 0 failed, 63 ignored (787 total), 528.94s.
- Task 7 focused suites:
  - manifest: 4 passed.
  - config: 3 passed.
  - certification: 6 passed.
  - equivalence: 9 passed.
- Integration regressions:
  - `compile_end_to_end`: 14 passed.
  - `reference_circuits`: 10 passed.
  - `seven_segment`: 3 passed.
- Simulator unit regressions: 19 passed.
- `cargo check --all-targets`: passed.
- `git diff --check`: clean.
- `cargo clippy --lib --tests -- -D warnings` reports only the two pre-existing unrelated warnings: simulator revision-descriptor `clone_on_copy` and planner test-only `field_reassign_with_default`. No Task 7 warning remains.

## Review findings fixed

1. Re-instantiation equality alone could certify a library entry whose topology had the wrong Boolean meaning. Added canonical symbolic primitive composition and a malicious NOR-as-repeater regression.
2. The initial simulator event cap was checked only after settling. Added bounded settling which refuses before processing work beyond the request-scoped cap.
3. The initial event accounting reset between source and destination. Both phases now consume one transition budget.
4. Candidate fingerprints initially had no explicit quality-selection contract. Added separate stable tie ordering and strict-improvement tests.
5. Symbolic input conversion used an infallible assumption. Oversized identity now returns `InputIdentityOverflow` instead of panicking.

## Modified files

- `src/compile/equivalence.rs`
- `src/compile/fragment_synth/certification.rs` (new)
- `src/compile/fragment_synth/config.rs` (new)
- `src/compile/fragment_synth/manifest.rs`
- `src/compile/fragment_synth/mod.rs`
- `src/compile/fragment_synth/timing_graph.rs`
- `src/compile/fragment_synth/topology.rs`
- `src/compile/topology.rs` (test-only library mutation seam)
- `src/redstone/simulator/mod.rs`

## Review availability

- Claude/Codex subagent quotas remained exhausted and CodeRabbit CLI remained unavailable. The final review was a controller review backed by the complete regression run.
- Production generation still uses the legacy front door. Task 8 may consume `ExpandedCandidateCertifier`; Task 13 alone may switch production and remove legacy generation after acceptance.
