# Task 9 implementation report

Status: `DONE`

## Outcome

- Added public `compile_fragment_synth`, `SynthesisInput`, evaluation/time `SynthesisBudget`, `SynthesisResult`, `SynthesisCaseFingerprint`, `ProposalTrace`, terminal outcomes, cap-work counters, and stop reasons.
- The explicit API constructs Task 8's durable services and calls the independent sparse-seed seam exactly once before the proposal loop. Evaluation budget zero therefore returns a completely certified seed with no proposal trace.
- Added `PlannerKind::FragmentSynth` only to this explicit API. Existing `compile`, `compile_grown`, CLI, baker, and viewer paths remain unchanged.
- Added a loop-boundary budget state machine. Evaluation budgets produce exact deterministic trace prefixes; time budgets finish an in-flight proposal and stop only at the next boundary; every refusal keeps the certified incumbent.
- The Task 9 production proposal stream is deliberately finite and non-mutating: two deterministic refused proposals per configured fragment-size schedule entry, eight under checked defaults. Tasks 10 and 11 replace these placeholders with real fragment transactions.
- A candidate is accepted only when its complete `QualityKey` is strictly smaller. Fingerprints do not count as improvement.

## Case identity

`SynthesisCaseFingerprint` canonically binds:

- ordered lowered netlist inputs, outputs, gates, and source provenance;
- ordered pinned IO coordinates and outside-facing directions;
- cell-library revision;
- every `SearchConfig` and `CertificationConfig` field;
- transition-manifest fingerprint;
- simulator revision;
- the expanded-strict physical-verifier revision.

The review found and fixed an initial use of the legacy verifier revision. A dedicated test now proves the legacy and expanded verifier authorities have distinct, stable revisions.

## Verification evidence

- Search state-machine suite: 5 passed.
- API suite: 2 passed, including complete fresh NOT synthesis at evaluation budgets 0, 1, 2, 4, and 8.
- Public architecture test: 1 passed in the final run in 72.49s. Two fresh and4 budget-zero runs had equal case/candidate fingerprints, byte-identical world cells and palettes, `PlannerKind::FragmentSynth`, empty traces, and complete 16-vector truth tables.
- Existing `compile_end_to_end`: 14 passed.
- `cargo check --all-targets`: passed.
- `cargo clippy --lib --tests`: no Task 9 warning; only the two pre-existing simulator/planner-test warnings remain.

## Review availability

- Reviewer subagent and CodeRabbit were unavailable on this host. The controller performed a full read-only diff review against Task 9 and fixed the expanded-verifier case-identity issue before commit.
