# Hierarchical Optimization Passes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement and retain multiple hierarchical optimization passes only when they reduce latency, block count, or occupied volume on a fully certified non-toy circuit.

**Architecture:** Extend the existing deterministic `HierarchicalProposalStream` with fixed stages. Placement stages rebuild and certify the hierarchical parent; route stages clone and minimally mutate the unioned flat candidate, refresh typed route metadata, and use the existing complete certifier.

**Tech Stack:** Rust, existing REDA fragment synthesis, typed route trees, realised timing graph, built-in certification and simulation.

**Spec:** `docs/superpowers/specs/2026-09-06-hierarchical-optimization-passes.md`

## Global Constraints

- Preserve budget-zero output, flat synthesis, pinned IO, and module compile-once/stamp-many behavior.
- Preserve exact deterministic evaluation prefixes and stop time budgets only between proposals.
- Do not change `QualityKey`, add dependencies, add a pass registry, or add configuration knobs.
- Run Cargo commands serially.
- Remove every part of the zero-yield parent-boundary-sharing prototype that the replacement no longer uses.

---

### Task 1: Remove the zero-yield sharing prototype

**Files:**
- Modify: `src/compile/fragment_synth/blocks.rs`
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Modify: `src/compile/fragment_synth/seed.rs`
- Modify: `src/compile/fragment_synth/union.rs`
- Modify: `src/compile/routing.rs`

**Interfaces:**
- Consumes: the alignment-only behavior at commit `dadef59`.
- Produces: alignment-only code with no `shared_inputs`, `required_root_strength`, or `TerminalRequirement::AutomaticAtLeast`.

- [ ] **Step 1: Preserve the replacement test target**

Rename the current end-to-end sharing test to
`input_seam_absorption_removes_the_child_refresh` and change its expectation
to a retained parent boundary repeater plus a removed child internal repeater.
It must fail before Task 3 supplies the replacement.

- [ ] **Step 2: Remove prototype plumbing**

Delete `required_root_strength`, `single_consumer_block_edges`,
`shareable_block_edges`, `shared_inputs`, `AutomaticAtLeast`, their
fingerprints, and their focused tests. Restore the original exact repeater
requirement for parent block-input terminals.

- [ ] **Step 3: Run the alignment regression**

Run:

```powershell
cargo test --lib evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases -- --nocapture
```

Expected: PASS. Keep the new seam test filtered out until Task 3.

- [ ] **Step 4: Commit the cleanup**

```powershell
git add src/compile/fragment_synth/blocks.rs src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/seed.rs src/compile/fragment_synth/union.rs src/compile/routing.rs
git commit -m "refactor: remove zero-yield boundary sharing"
```

### Task 2: Add one-cell Block Pull X proposals

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Test: `src/compile/fragment_synth/hierarchy_api.rs`

**Interfaces:**
- Consumes: `BlockEdge`, compiled port maps, realised block offsets, and cumulative `BlockPlacementOffset` values.
- Produces: `block_pull_x_proposal(edge, source_outputs, sink_inputs, realised_offsets, incumbent) -> Option<BTreeMap<InstanceId, BlockPlacementOffset>>` and a Pull-X stream stage.

- [ ] **Step 1: Write the failing pure proposal test**

Use `block_proposal_fixture()` to assert that the selected sink moves exactly
one X cell toward the realised source port, preserves its accepted Z offset,
does not move any other block, and returns `None` when X is already equal.

- [ ] **Step 2: Verify the test fails**

Run:

```powershell
cargo test --lib block_pull_x_moves_only_the_sink_one_cell_toward_the_source -- --nocapture
```

Expected: FAIL because `block_pull_x_proposal` does not exist.

- [ ] **Step 3: Implement the minimal proposal**

Compute global source and sink X from block-local port cells plus realised
offsets. Add only `signum(source_x - sink_x)` to the sink's cumulative `dx`
and preserve `dz`. Return `None` for zero delta.

- [ ] **Step 4: Append the stage**

After alignment proposals, enumerate the same ordered edges as Pull-X
proposals. Add distinct fragment and choice fingerprint schemas while keeping
one `run_budgeted_proposals` invocation.

- [ ] **Step 5: Run focused tests and commit**

```powershell
cargo test --lib block_pull_x -- --nocapture
cargo test --lib evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases -- --nocapture
git add src/compile/fragment_synth/hierarchy_api.rs
git commit -m "feat: pull connected modules closer"
```

Expected: tests PASS; the original alignment trace remains the first prefix.

### Task 3: Add Input Seam Absorption

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Modify: `src/compile/fragment_synth/union.rs`
- Test: `src/compile/fragment_synth/union.rs`
- Test: `src/compile/fragment_synth/hierarchy_api.rs`

**Interfaces:**
- Consumes: each sink block's compiled input route and `union_candidate`'s per-instance clone.
- Produces: `InputSeamChoice { sink_block: InstanceId, input: u16, at: Anchor }`, cumulative accepted seam choices, and explicit refusal of stale/unsafe choices.

- [ ] **Step 1: Write a failing strength test**

Build a small input route with a retained boundary repeater and two branches.
Assert that replacing the first non-terminal repeater with dust is accepted
when both branches remain within strength 15, and refused when one branch
reaches zero before its next refresh or sink.

- [ ] **Step 2: Verify red**

```powershell
cargo test --lib seam_absorption_requires_every_branch_to_keep_signal -- --nocapture
```

Expected: FAIL because the helper does not exist.

- [ ] **Step 3: Implement the strength walk**

Index route cells by anchor. Begin with strength 15 after the retained parent
boundary repeater, decrement for dust, restore 15 for retained repeaters, and
reject missing or non-conductor cells, terminal repeaters, or zero strength.

- [ ] **Step 4: Write the failing stamping test**

Stamp two instances of one block, apply a seam choice to only one, and assert
that only its selected block-local repeater becomes `crate::compile::dust()`.
Paths, floors, the second instance, and the original `CompiledBlock` remain
unchanged.

- [ ] **Step 5: Apply the choice to the clone**

In `union_candidate`, after cloning and before translation, require the anchor
to be a non-terminal route-owned repeater on the named primary-input route.
Replace its complete state with `dust()` and refuse a stale descriptor.

- [ ] **Step 6: Refresh and certify**

Reuse `normalise_routes_and_connections` and `refresh_exact_route_delays`.
Complete the full-adder-chain test so it proves the parent boundary repeater
remains, the child repeater becomes dust, total repeaters fall, and unchanged
full certification accepts the proposal.

- [ ] **Step 7: Run focused tests and commit**

```powershell
cargo test --lib seam_absorption -- --nocapture
cargo test --lib input_seam_absorption_removes_the_child_refresh -- --nocapture
git add src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/union.rs
git commit -m "feat: absorb redundant module input refreshes"
```

Expected: PASS.

### Task 4: Add parent-route repeater pruning

**Files:**
- Create: `src/compile/fragment_synth/route_opt.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Test: `src/compile/fragment_synth/route_opt.rs`

**Interfaces:**
- Consumes: a cloned `ExpandedPhysicalCandidate`, `RouteId`, realised timing graph, and pre-union parent-owned route anchors.
- Produces: `prune_parent_route(candidate: &mut ExpandedPhysicalCandidate, route: RouteId, mutable: &BTreeSet<Anchor>) -> Result<bool, RouteOptRefusal>`.

- [ ] **Step 1: Write failing pruning tests**

Cover a straight route where two refreshes become one, a fanout where a branch
would lose signal, terminal repeater exclusion, and child-owned anchor
exclusion. Only the straight safe route returns `true`.

- [ ] **Step 2: Verify red**

```powershell
cargo test --lib route_opt::tests::prune -- --nocapture
```

Expected: FAIL because `route_opt` does not exist.

- [ ] **Step 3: Implement deterministic pruning**

Collect internal route-owned repeaters on mutable anchors, excluding branch
terminals. Sort by greatest path index then anchor. Tentatively replace each
with dust, keep it only when every affected branch passes the strength proof,
and call `refresh_exact_route_delays` after retained changes.

- [ ] **Step 4: Add low-slack descriptors and certification**

Use timing arcs of kind `TimingArcKind::Route`, deduplicate route IDs, sort by
minimum analysed slack then route ID, and freeze descriptors after the seam
stage. Clone the incumbent flat candidate, prune one route, return `Refused`
when unchanged, and otherwise call existing `certify_planned`.

- [ ] **Step 5: Run focused tests and commit**

```powershell
cargo test --lib route_opt -- --nocapture
cargo test --lib deterministic_quality_staircases -- --nocapture
git add src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/hierarchy_api.rs
git commit -m "feat: prune redundant critical-route repeaters"
```

Expected: PASS with deterministic trace prefixes.

### Task 5: Add bounded refresh relocation

**Files:**
- Modify: `src/compile/fragment_synth/route_opt.rs`
- Modify: `src/compile/fragment_synth/hierarchy_api.rs`
- Test: `src/compile/fragment_synth/route_opt.rs`

**Interfaces:**
- Consumes: parent-owned restrictions and the strength proof from Task 4.
- Produces: `relocate_refresh_to_remove_next(...) -> Result<bool, RouteOptRefusal>` used only when direct pruning makes no change.

- [ ] **Step 1: Write failing relocation tests**

Create a straight route where direct deletion fails but moving the common
upstream repeater downstream allows the next repeater to be deleted. Add
refusals for a corner, different branch predecessors, and a child-owned
destination.

- [ ] **Step 2: Verify red**

```powershell
cargo test --lib relocate_refresh -- --nocapture
```

Expected: FAIL because relocation does not exist.

- [ ] **Step 3: Implement one bounded relocation**

Require every affected branch to share the same predecessor refresh and the
destination to be a straight parent-owned dust cell. Try destinations from
downstream to upstream. Move a minimum-delay repeater with facing derived from
the path, remove the downstream refresh, and retain only a strength-valid
net-minus-one result. Attempt at most one relocation per route proposal.

- [ ] **Step 4: Run focused tests and commit**

```powershell
cargo test --lib route_opt -- --nocapture
git add src/compile/fragment_synth/route_opt.rs src/compile/fragment_synth/hierarchy_api.rs
git commit -m "feat: repack critical-route refreshes"
```

Expected: PASS.

### Task 6: Measure, retain, or delete each pass

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs` only if the ignored oracle needs volume or stage reporting.
- Create: `.superpowers/sdd/2026-09-06-hierarchical-optimization-passes/report.md`

**Interfaces:**
- Consumes: the complete finite stream and existing large-circuit harnesses.
- Produces: a per-pass retention decision backed by certified metrics.

- [ ] **Step 1: Run the library regression serially**

```powershell
cargo test --lib
```

Expected: all deterministic tests pass. If the known 100 ms poison-recovery
test alone is the only full-load failure, rerun that exact test alone and
record both outcomes instead of changing its timeout.

- [ ] **Step 2: Run ripple8 to exhaustion**

```powershell
cargo test --release --lib ripple_adder8_hierarchical_budget_target_oracle -- --ignored --nocapture
```

Record each accepted stage's observed settle, blocks, volume, static delay,
evaluation count, and wall time against the 576-tick alignment incumbent.

- [ ] **Step 3: Measure the other large circuits**

Run the existing hierarchical acceptance/oracle commands for `alu4_full`,
`multiplier4`, and `alu8`, one Cargo process at a time, and record the same
metrics plus certification status.

- [ ] **Step 4: Enforce retention**

Retain Pull-X, seam absorption, direct pruning, or relocation only if at least
one non-toy circuit accepts a strictly better candidate. Delete zero-yield
code and tests with `apply_patch`; leave no disabled flags or dead interfaces.

- [ ] **Step 5: Final verification**

Run serially:

```powershell
cargo test --lib
cargo clippy --lib --tests
```

Then run the existing six-case hierarchical acceptance, pinned seven-segment
IO contract, flat 16-extra, and flat four-large harnesses. Do not run global
`cargo fmt --all -- --check`; this repository has a baseline-wide rustfmt
mismatch. Format only touched Rust files when it does not rewrite unrelated
code.

- [ ] **Step 6: Review and final commit**

Request independent correctness and over-engineering reviews. Fix only
evidence-backed findings, rerun affected focused tests, show the final diff,
and commit the retained implementation and report.
