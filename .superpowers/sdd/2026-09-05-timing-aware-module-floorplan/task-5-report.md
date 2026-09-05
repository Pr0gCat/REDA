# Task 5 report — regression and review

## Result

The module-floorplan/search range `addfe03..e3ce4f5` preserves certification,
equivalence, simulation, pinned IO geometry, and the existing flat synthesis
path.  The timing target remains a bounded negative result: the exhausted
hierarchical search reaches 576 ticks / 71,147 blocks, versus the required
474 / 100,615.

## Verification

- `cargo test --release --lib`: 892 passed, 0 failed, 68 ignored.
- `cargo test --release --tests`: all binaries reached before
  `fragment_synth_baseline` passed.  The baseline binary stopped the aggregate
  run on a stale verifier-revision fingerprint; rerunning it on a clean tree
  gave 2 passed / 1 failed, with only
  `1af206... != 049a81...`.  The relevant source and fixture blobs are
  identical at `addfe03`; `b1ddbe5` changed verifier authority after the last
  fixture refresh, so this is pre-existing and is not repaired in this goal.
- Every integration binary after that stop was run separately and passed:
  hierarchy, litematic, primitive equivalence, reference circuits, seven
  segment, simulator, spacing, terminal handover, timing, and Verilog suites.
- `every_hierarchical_circuit`: passed all four cases.  Key rows:
  `ripple_adder8` 608 ticks / 70,603 blocks; `alu4_full` 925 / 191,062;
  `multiplier4` 1,039 / 124,948; three-level `alu8` 972 / 213,833.
- `every_extra_circuit`: all 16 flat circuits passed in 155.67s.
- `every_large_circuit`: all four flat circuits passed in 1,248.43s and
  reproduced `ripple_adder8` at 474 ticks / 100,615 blocks.
- `fragment_acceptance`: completed in about 51m22s.  All 30 case/budget runs
  compiled and certified; all 30 failures were the old replacement quality
  gate, not correctness.  No shipping configuration was generated.  This is
  the known topology-aware-vs-legacy quality gap, outside the hierarchical
  path changed here.
- `cargo clippy --lib`: completed with no errors.  Warnings are existing
  dead-code/API-shape warnings; no broad cleanup was mixed into this goal.

## Review

- Boundary/union reviewer: APPROVED; no Critical or Important finding.  One
  deferred Minor notes that proposal evaluation reconstructs an
  `InstanceGraph`; caching it is not justified without a measured bottleneck.
- Evidence/simplicity reviewer: implementation and bounded claims were sound;
  its only Important finding was the absence of this Task 5 evidence record,
  now addressed.
- CodeRabbit CLI was unavailable, so no external review was claimed.  Two
  Claude Code reviewers completed independently; a slower Opus review was
  stopped after producing no result rather than delaying closure.

## Conclusion

The implemented algorithm is complete and certified as a deterministic,
interruptible module-floorplan quality staircase.  It does not meet the 474
tick target under the current module boundary contract.  The next smallest
target-relevant design is general double-`Nor(1)` output-alias elimination in
hierarchy flattening; single-consumer repeater sharing is specified separately
but has only a 14-tick structural ceiling on ripple-adder carry edges.
