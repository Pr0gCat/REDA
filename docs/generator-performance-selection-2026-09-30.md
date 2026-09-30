# Performance-only candidate selection

Problem: while preparing typed torch-matrix integration, the existing
`QualityKey` derived total ordering over observed settling, non-air blocks,
occupied volume, then static routed delay. Equal-settling candidates could be
accepted merely because they used fewer blocks. A larger candidate with lower
static delay could be rejected for its size. Both conflict with the requested
quality policy that block count and volume are diagnostic rather than standards.

The common performance key now contains only observed settling and static routed
delay. CandidateMetrics improvement and stable selection, and the test-only
budgeted proposal loop, use that key. Fingerprints still break performance ties
for deterministic ordering without turning a tie into an improvement. The
production recursive producer currently has no budgeted proposal-selection loop;
this fixes the public metric methods and the retained proposal mechanism rather
than claiming that shipping worlds have already changed.

The size fields remain present in QualityKey serialization and physical metrics,
and the existing evaluation consistency checks continue to verify them. The
raw struct no longer implements Ord/PartialOrd: callers must explicitly select
`performance_key()` rather than accidentally sorting diagnostics. This is an
intentional source-level API change; reported metric fields are unchanged.

Regression coverage exercises equal-time smaller candidates (not improvements),
larger candidates with lower static delay (improvements), fingerprint tie
ordering, and the proposal loop retaining an incumbent for size-only changes.
Existing certification, proof, physical simulation and quality caps are intact.
No gate count or occupied volume has been substituted elsewhere as a quality key.
This change does not add hazard metrics to the fragment certifier, replace the
independent driver/lamp quality checks, or adopt the torch matrix in compile().

Validation completed:

- `cargo test --release --lib compile::fragment_synth::search::tests`: 5 passed.
- `cargo test --release --lib compile::fragment_synth::certification::`: 19 passed,
  including physical support, function/corruption, complete certification, and
  deterministic worker outcomes.
- `cargo test --release --lib compile::fragment_synth::api::tests`: 14 passed.
- `cargo test --release --lib compile::fragment_synth::fragment::tests`: 9 passed,
  including actual certified fragment transactions and incumbent preservation.
- `cargo test --release --test frontend_quality_report --test
  generator_quality_matrix`: both passed, 1312 measured physical transitions,
  final driver/lamp truth, independent historical/current quality caps and
  repeat-generation determinism. No acceptance metric was changed.
- `cargo check -p reda --target wasm32-unknown-unknown`: passed.
- `git diff --check`: passed.

These are scoped checks, not a claim that the full repository suite passes.
Existing packing.rs unused_mut and lib-test rank_zero dead-code warnings remain.
The change corrects improvement/selection behavior; it does not claim lower
observed settling in current shipping circuit fixtures. Torch-matrix integration
still requires faithful topology/net mapping and the existing physical verifier
pipeline before production adoption.
