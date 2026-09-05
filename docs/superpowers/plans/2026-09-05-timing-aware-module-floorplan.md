# Timing-aware module floorplan implementation plan

**Goal:** Make hierarchical time/evaluation budgets optimise real module
connections and try to reach 474 ticks / 100,615 blocks on programmatic
`ripple_adder8` without weakening certification or flat/pinned behaviour.

**Architecture:** Reuse the existing budget runner and full candidate
certification.  Add only a hierarchical block-offset candidate family: stable
critical-edge order, one sink-port alignment per proposal, fresh routing and
certification per candidate.

---

### Task 1: Block placement override

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs`
- Test: `src/compile/fragment_synth/seed.rs`

1. Add a failing test that supplies an offset for one block and asserts its
   body and every registered port move by exactly that offset while another
   block does not move.
2. Pass a separate block-offset map into `plan_parent_with_services` and
   `place_blocks`; apply it before occupancy claims and source/target
   registration.
3. Run the focused unit test and existing block-placement tests.

### Task 2: Deterministic parent-edge proposals

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Test: `src/compile/fragment_synth/hierarchy_api.rs`

1. Add failing pure tests for stable block-edge ordering and cumulative
   source-output/sink-input alignment.
2. Extract explicit `BlockEdge` values only from a sink assignment whose
   driver has one primitive terminal owned by a real source block; validate
   both endpoint IDs with `InstanceGraph::block`.
3. Wrap each search incumbent as `{ certified, block_placements,
   realised_block_offsets }`, so only a candidate accepted by
   `run_budgeted_proposals` can become the parent of the next move.
4. Implement a private proposal stream using compiled port tables and the
   wrapper's accepted offsets.
5. Replace `FragmentProposalStream` only on the hierarchical top path; keep
   the flat path unchanged.
6. Assert proposals never target flattened block-internal gates and never
   request duplicates.

### Task 3: Budget contract

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Test: `src/compile/fragment_synth/hierarchy_api.rs`

1. Add focused 0/1/2/4 tests proving smaller traces are exact prefixes and the
   selected `QualityKey` never worsens.
2. Prove worker counts 1 and 4 return the same fingerprint for an evaluation
   budget.
3. Reuse the existing time-budget boundary test; do not add a second clock or
   scheduler.

### Task 4: Measure the target

**Files:**
- Modify only if needed: `src/compile/fragment_synth/seed.rs` ignored release
  harness

1. Add a dedicated release harness that runs programmatic hierarchical
   `ripple_adder8` at budgets 0, 1, 2, 4 and true stream exhaustion with
   `--release --nocapture`.
2. Record ticks, blocks, static delay, trace terminals and fingerprints.
3. Assert success only when the same certified best has both
   `observed_settle <= 474` and `non_air_blocks <= 100_615`. Otherwise assert
   `StopReason::ProposalStreamExhausted`, record the exhausted-stream bounded
   result, and proceed only to the documented repeater-sharing design.

### Task 5: Regression and review

1. Run focused unit tests, `cargo test --release --lib`, pinned IO, channel
   safety, fragment acceptance, architecture, reference and terminal-handover
   suites, serially.
2. Run the flat 16-extra and four-large harnesses serially and compare their
   fingerprints/metrics with the checked-in baseline.
3. Run `cargo clippy --lib`.
4. Review the final diff for accidental flat-path or boundary-contract changes
   before committing.
