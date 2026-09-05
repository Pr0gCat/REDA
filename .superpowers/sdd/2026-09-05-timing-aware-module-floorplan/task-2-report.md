# Task 2 report: deterministic parent-edge proposals

## Changed files and symbols

- `src/compile/fragment_synth/hierarchy_api.rs`
  - `BlockEdge` and `explicit_block_edges` turn only validated physical
    block-to-block primitive assignments into stable `(slack, source block,
    sink block, sink input)` proposals.  Each `SinkAssignment` remains an
    edge; pair-level slack comes from `EdgeFacts`.
  - `compiled_port_lookup` follows `CompiledBlock.lowered.inputs` and
    `.outputs` declaration order, rather than map iteration.
  - `block_alignment_proposal` clones the accepted block-placement map,
    updates only the selected sink's Z displacement, preserves X, and uses
    compiled port cells plus realised parent offsets to make the chosen ports
    share absolute Z.
  - `HierarchicalCandidate` retains the certified flat candidate, accepted
    placement map, and realised block offsets.  Its `SearchCandidate`
    implementation delegates fingerprint and quality to the certified
    candidate.
  - `HierarchicalProposalStream` is finite and private.  It compiles one
    stable block edge per proposal through the existing parent plan, union and
    complete certifier; the flat no-instance path remains unchanged.
  - `HierarchicalProposalFingerprint` serializes a versioned descriptor with
    the edge, incumbent fingerprint and ordered `(InstanceId, dx, dz)` tuples.
  - Tests: `explicit_block_edges_accept_only_valid_block_terminals_and_sort_stably`,
    `block_alignment_proposal_is_cumulative_and_moves_only_its_sink`, and the
    corrected `a_non_zero_budget_evaluates_real_proposals` integration test.
- `src/compile/fragment_synth/fragment.rs`
  - `terminal_for_seed_error` is `pub(crate)`, so hierarchy proposals preserve
    the established refusal-terminal and cap-counter classification.

## RED/GREEN evidence

RED was intentional.  Running
`cargo test --lib explicit_block_edges_accept_only_valid_block_terminals_and_sort_stably -- --nocapture`
failed at compile time only because `BlockEdge`, `explicit_block_edges`, and
`block_alignment_proposal` did not yet exist (`E0422`/`E0425`, seven errors).

The controller then verified the final production code:

- `explicit_block_edges_accept_only_valid_block_terminals_and_sort_stably`:
  pass; 1 passed, 957 filtered, 0.00 s.
- `block_alignment_proposal_is_cumulative_and_moves_only_its_sink`: pass; 1
  passed, 957 filtered, 0.00 s.
- `a_parent_refuses_only_the_proposals_it_cannot_represent`: pass; 1 passed,
  957 filtered, 11.53 s.
- Initial `hierarchy_api` suite: 10 passed, 1 ignored, and one stale
  integration assertion failed after 85.10 s.  Its `three_level_design` top
  has only one block, so correct block-edge extraction exhausted with zero
  evaluations.
- After the fixture correction, `a_non_zero_budget_evaluates_real_proposals`:
  pass; 1 passed, 957 filtered, 212.02 s.

The correction uses `ripple_adder(2)`, which has a real block-to-block carry
edge, compares budget one against its budget-zero baseline, and asserts one
trace entry and one evaluation.  It retains output and non-worsening-quality
checks, but no longer requires a certified proposal: a classified compile
refusal is valid evidence that the finite stream called the parent compiler.
Together with the final focused pass, every nonignored `hierarchy_api` test is
covered at final production code.  The only warning in these runs was the
pre-existing `BlockFacts.delay_ticks` dead-code warning.

## Review against the brief and minimality

- Source identity requires a single primitive terminal, matching logical and
  terminal owners, a validated source block, output-node bounds, and a signal
  equal to that block output gate.  The sink must be a validated block input
  in range.  Primary input, junction, multi-terminal, owner mismatch, invalid
  ports, signal mismatch, and non-block endpoints are rejected by the compact
  fixture.
- Rejected and non-improving candidates cannot mutate the incumbent because
  only `run_budgeted_proposals` can replace the whole wrapper.
- The stream carries only block placements; it does not carry `SeedVariant`
  choices, request gate placements, or request duplicates.  Existing parent
  representability guards stay ahead of planning-graph construction.
- Fingerprints do not depend on Rust `Debug` output: their JSON sidecar has a
  schema tag and deterministic ordered data.  Compile failures reuse
  `terminal_for_seed_error`, preserving router, backtrack, proof, verification
  and certification-cap accounting.
- Ponytail review: the implementation reuses the existing budget runner,
  compiler, certifier, port tables, `BTreeMap`, `serde_json`, and error
  mapping.  No generic optimizer, scheduler, router, certifier, dependency,
  boundary-repeater change, or new source file was added.

## Limitations and follow-up boundaries

- This task establishes deterministic edge extraction and one-edge alignment;
  Task 3 owns exhaustive prefix/stop-reason behavior and Task 4 owns measured
  success rates and the `ripple_adder8` latency/block target.
- A proposal moves only its selected sink in Z.  It intentionally does not
  alter X, gate placements, duplication, router behavior, or boundary
  repeaters.
- The stream may legitimately exhaust or classify a proposal refusal.  That
  is not a latency-target pass; if Task 4 measures exhaustion above target,
  the specified next experiment is constrained single-consumer boundary
  repeater sharing.

## Local final checks

This finalization ran `rustfmt --edition 2021` on both Rust files and
`git diff --check` successfully.  Cargo was not run during finalization by
request; the test results above are controller-provided verification evidence.
