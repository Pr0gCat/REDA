# Task 10 report

Implemented deterministic timing-guided single-instance fragment transactions.

## Design delivered

- Stable hotspot order by static slack, dynamic activation, then `TimingArcId`.
- Complete shared route-tree closure with typed boundary endpoints.
- Canonical `FragmentId` and `FragmentChoice` fingerprints.
- Bounded implementation, facing, Manhattan-shell, router, proof, verifier, and certification outcomes.
- Cumulative incumbent `SeedVariant` state keyed by certified candidate fingerprint.
- Fresh materialise-route-emit-verify-certify transactions; only strict `QualityKey` improvements replace best.
- Primitive unused-input keep-outs prevent support-powered cross-route coupling after rotation.

The transaction currently rematerialises the complete independent candidate from cumulative typed variant state. This is intentionally stronger and simpler than mutating candidate records in place, at the cost of incremental compile speed.

## Verification

- Fragment tests: 7 passed.
- Search tests: 5 passed.
- API tests: 3 passed.
- Independent architecture test: 1 passed.
- Seed tests including pinned IO: 11 passed in 192.12s.
- Targeted rustfmt check and `git diff --check`: passed.
- Clippy passed with only the pre-existing `clone_on_copy` lint in `src/redstone/simulator/mod.rs` explicitly allowed; no Task 10 lint remained.

Atomic failure coverage forces placement/materialisation, router, physical verifier, equivalence proof, certification transition, and fragment backtrack failures, then checks that parent candidate and timing graph fingerprints remain unchanged. A separate two-instance test proves a later certified transaction accumulates the earlier incumbent variant.

CodeRabbit CLI was unavailable in this environment. Manual diff review found and fixed the non-cumulative incumbent bug before commit.
