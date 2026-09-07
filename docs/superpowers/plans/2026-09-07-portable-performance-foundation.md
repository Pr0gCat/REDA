# Portable Performance Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure the complete hierarchical proposal pipeline, remove duplicate certification work, and hoist immutable hierarchical artifacts so every candidate performs one authoritative certification transaction.

**Architecture:** Preserve the current planner, router, union, simulator and search order. Route one emitted and physically verified world through the rest of certification, compute candidate identity once, and keep top-module flattening and plan-derived pruning data beside the immutable context that owns them. This is the independently shippable Layer 0-1 foundation; parallel exhaustive certification, router overlays and incremental simulation each receive a later plan after this layer passes its retention gate.

**Tech Stack:** Rust standard library, existing REDA fragment-synthesis services, scoped timing with `std::time::Instant`, Cargo tests and release acceptance harnesses.

**Spec:** `docs/superpowers/specs/2026-09-07-portable-extreme-generation-performance.md`

## Global Constraints

- The portable CPU implementation remains complete and efficient with one worker; no GPU or new dependency in this plan.
- Preserve candidate/case/emitted-world fingerprints, proposal traces, `QualityKey`, cap accounting, deterministic errors, pinned IO and certification measurements.
- Keep full structural verification, physical verification, equivalence, exhaustive truth, manifest simulation and timing analysis.
- Do not change router costs, neighbour order, tie-breaks, route geometry, pass order or budget boundaries.
- Serialize Cargo commands in this shared Windows target directory.
- Use the current worktree's incumbent `Arc<PlannedParent>` reuse as the starting behavior; do not replace it with a global cache.
- Retain the milestone only if its targeted phase is at least 1.5x faster, no representative point regresses more than 5%, and all correctness gates pass.
- Invoke Claude Code with an explicit model on every agent session: Opus 5 is
  the primary implementation, difficult-debugging and high-risk-review model;
  Sonnet handles bounded mechanical call-site/test updates; Fable 5.1 is
  reserved for cheap read-only lookup or log summarization and must not write
  production code.
- Use `--model opus --effort high` for Tasks 3-5, raising to `xhigh` for a
  genuinely unclear correctness failure. Use `--model sonnet --effort medium`
  only for a task already reduced to explicit mechanical edits.

---

## File Map

- Modify `src/compile/fragment_synth/seed.rs`: remove the disposable pre-certification emit/verify pass and retain top-level phase boundaries.
- Modify `src/compile/fragment_synth/certification.rs`: own the single complete certification transaction and record its internal phases.
- Delete `src/compile/fragment_synth/services.rs`: remove the duplicate finishing facades and import the placer from its owning module.
- Modify `src/compile/fragment_synth/timing_graph.rs`: consume the already-computed certified candidate identity.
- Modify `src/compile/equivalence.rs`: consume the same candidate identity instead of serializing the candidate again.
- Modify `src/compile/fragment_synth/hierarchy_api.rs`: own immutable top flattening and plan-derived route-pruning data once.
- Create `docs/superpowers/reports/2026-09-07-portable-performance-foundation.md`: record commands, samples, phase counts, fingerprints and the KEEP/REVERT decision.

### Task 1: Freeze the incumbent-plan prerequisite

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs:337-440, 764-794, 2789-2930`
- Modify: `src/compile/fragment_synth/seed.rs:84-88`

**Interfaces:**
- Consumes: current uncommitted `HierarchicalCandidate::planned: Arc<PlannedParent>` and `BlockPlacementOffset: PartialEq + Eq`.
- Produces: a separately committed, verified baseline in which unchanged block placements reuse the incumbent plan but still run union and complete certification.

- [ ] **Step 1: Have Opus inspect the existing diff and test contract**

Run a fresh read-only reviewer with `claude --model opus --effort high`. Give it
the current diff and require an explicit verdict on stale-plan keys, changed
placement replanning, rejected-proposal behavior, full recertification and
memory ownership. Fable output from the earlier implementation is not an
approval for this step.

Then inspect:

Run:

```powershell
git diff -- src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/seed.rs
```

Confirm the test covers baseline planning, changed-placement replanning, rejection of that moved proposal, later incumbent-plan reuse, pointer identity and an additional certifier call.

- [ ] **Step 2: Re-run the focused prerequisite test**

Run:

```powershell
cargo test --lib compile::fragment_synth::hierarchy_api::tests::unchanged_block_placements_reuse_the_incumbent_plan
```

Expected: one passed test. Do not accept an agent's earlier output as evidence.

- [ ] **Step 3: Commit only the prerequisite files**

```powershell
git add -- src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/seed.rs
git diff --cached --check
git commit -m "perf: reuse routed hierarchical parent plans"
```

Expected: the performance spec/plan commits remain separate and no unrelated file is staged.

### Task 2: Expose complete phase timing and work counts

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs:624-772`
- Modify: `src/compile/fragment_synth/certification.rs:183-261, 268-437`
- Modify: `src/compile/fragment_synth/hierarchy_api.rs:403-440`
- Create: `docs/superpowers/reports/2026-09-07-portable-performance-foundation.md`

**Interfaces:**
- Consumes: existing opt-in `REDA_PHASE_TIMING` behavior.
- Produces: stable diagnostic lines `PHASE union`, `PHASE structure+emit+verify`, `PHASE timing`, `PHASE equivalence`, `PHASE exhaustive`, `PHASE manifest`, `PHASE metrics+fingerprints`, plus counts for exhaustive vectors and manifest transitions.

- [ ] **Step 1: Capture the RED diagnostic gap**

Run:

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib compile::fragment_synth::hierarchy_api::tests::unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture
```

Expected before implementation: output has `PHASE layout+routing`, `PHASE emit+verify` and `PHASE certify`, but does not separate union, equivalence, exhaustive, manifest or fingerprint work. Save the complete output in the report as the pre-change sample; do not hand-edit times.

- [ ] **Step 2: Add minimal opt-in timers at existing boundaries**

Use the existing local `Instant`/closure pattern. Do not create a telemetry subsystem. In `union_and_certify`, time only `union_candidate` and emit:

```rust
if std::env::var_os("REDA_PHASE_TIMING").is_some() {
    eprintln!("PHASE union {}", union_started.elapsed().as_millis());
}
```

Inside `CompleteCandidateCertifier::certify`, reset one `Instant` after each named phase and print vector/transition counts with stable prefixes:

```rust
eprintln!("WORK exhaustive_vectors {state_count}");
eprintln!("WORK manifest_transitions {transition_count}");
```

Normal output remains unchanged when the environment variable is absent. Timing/count text never enters a fingerprint or result type.

- [ ] **Step 3: Verify the diagnostic labels**

Run the Step 1 command again.

Expected: every applicable label appears in pipeline order; the reused proposal contains no placement/routing phase; the test still passes. A circuit above the exhaustive-input threshold may omit or report zero exhaustive work, but must report its manifest count.

- [ ] **Step 4: Run focused certification tests**

```powershell
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: all non-ignored certification tests pass.

- [ ] **Step 5: Commit observability**

```powershell
git add -- src/compile/fragment_synth/seed.rs src/compile/fragment_synth/certification.rs src/compile/fragment_synth/hierarchy_api.rs docs/superpowers/reports/2026-09-07-portable-performance-foundation.md
git diff --cached --check
git commit -m "perf: expose certification phase timings"
```

### Task 3: Make certification the only emit/verify transaction

**Files:**
- Delete: `src/compile/fragment_synth/services.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/fragment_synth/certification.rs:171-261, tests around 780-850`
- Modify: `src/compile/fragment_synth/seed.rs:70-78, 741-772, tests around 3620-3775`
- Modify mechanical service imports/initializers in `src/compile/fragment_synth/api.rs`, `fragment.rs` and `hierarchy_api.rs`.

**Interfaces:**
- Consumes: `SeedEmitter`, `SeedVerifier` and the unchanged `ExpandedCandidateCertifier` interface.
- Produces the smaller finishing service set:

```rust
pub(crate) struct SeedServices<'a> {
    pub library: &'a Library,
    pub placer: &'a dyn SeedPlacer,
    pub router: &'a dyn PhysicalRouter,
    pub certifier: &'a dyn ExpandedCandidateCertifier,
    pub search_config: &'a SearchConfig,
}
```

`CompleteCandidateCertifier::certify` remains the sole durable implementation that calls `realise_and_verify_expanded`.

- [ ] **Step 1: Write the failing one-transaction service test**

Replace the broad existing service assertion with a wished-for `SeedServices` initializer that has no emitter/verifier fields, and rename the test:

```rust
#[test]
fn production_seed_has_one_finishing_authority() {
    let certifier = CountingCertifier::default();
    let certified = compile_sparse_seed_with_services(input, SeedServices {
        library: &library,
        placer: &TopologyAwareSeedPlacer,
        router: &router,
        certifier: &certifier,
        search_config: &config,
    }).expect("fixture certifies");

    assert_eq!(certifier.calls.get(), 1);
    assert_eq!(
        certified.metrics().candidate_fingerprint,
        certified.candidate().fingerprint(),
    );
}
```

- [ ] **Step 2: Run RED**

```powershell
cargo test --lib production_seed_has_one_finishing_authority
```

Expected: compile failure because `SeedServices` still requires emitter and verifier fields.

- [ ] **Step 3: Delete the disposable finishing path**

Delete the adapter/emitter/verifier block from `SparseSeedBuilder::finish_attempt`:

```rust
candidate.validate_shape()?;
candidate.validate_physical_ownership()?;
let certification = CertificationConfig::from_search(services.search_config);
services
    .certifier
    .certify(candidate, input.lowered, services.library, &certification)
```

Do not add a replacement preflight. `CompleteCandidateCertifier` already calls `realise_and_verify_expanded`, which runs structural validation, adapter, emission and durable physical verification once.

- [ ] **Step 4: Remove the unused service layer**

Remove emitter/verifier fields from `SeedServices`, delete their traits and durable unit structs, and update every initializer. Import `TopologyAwareSeedPlacer` directly from `placement`; if `services.rs` is empty, delete it and remove its module declaration. Replace `RefusingVerifier` tests with an `ExpandedCandidateCertifier` that returns the same physical error only where the test explicitly exercises transaction atomicity.

- [ ] **Step 5: Preserve complete-certifier authority coverage**

Keep `complete_certification_seals_structure_function_manifest_and_metrics` as the proof that the remaining authority performs structure, function, manifest and metrics. The seed test asserts exactly one certifier call:

```rust
assert_eq!(certifier.calls.get(), 1);
```

- [ ] **Step 6: Run GREEN and refusal tests**

```powershell
cargo test --lib production_seed_has_one_finishing_authority
cargo test --lib complete_certification_seals_structure_function_manifest_and_metrics
cargo test --lib compile::fragment_synth::fragment::tests::every_bounded_fragment_failure_keeps_the_parent_candidate_atomic
```

Expected: all pass; refusing physical verification still yields the same typed terminal and does not mutate the incumbent.

- [ ] **Step 7: Compare certified identity**

Run the focused hierarchy test with `REDA_PHASE_TIMING=1`. Expected: candidate and emitted-world assertions pass, one `structure+emit+verify` phase replaces the previous disposable `emit+verify` plus certification-internal realisation.

- [ ] **Step 8: Commit the single transaction**

```powershell
git add -- src/compile/fragment_synth/mod.rs src/compile/fragment_synth/services.rs src/compile/fragment_synth/certification.rs src/compile/fragment_synth/seed.rs src/compile/fragment_synth/api.rs src/compile/fragment_synth/fragment.rs src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "perf: certify each candidate through one physical transaction"
```

### Task 4: Compute candidate certification identity once

**Files:**
- Modify: `src/compile/fragment_synth/verify.rs:80-112`
- Modify: `src/compile/fragment_synth/realise.rs:446-504`
- Modify: `src/compile/fragment_synth/timing_graph.rs:100-125`
- Modify: `src/compile/equivalence.rs:290-425`
- Modify: `src/compile/fragment_synth/certification.rs:183-261`

**Interfaces:**
- Consumes: the `StructuralCertificate` produced by the one-pass realisation transaction.
- Produces:

```rust
#[derive(Debug, Clone)]
pub(crate) struct CertificationIdentity {
    pub candidate: Fingerprint,
    pub library_revision: Fingerprint,
}
```

`CertificationIdentity` is constructed once at certification entry and borrowed by structural certification, timing derivation and equivalence proof. Certificates continue to own cloned fingerprints so public lifetimes do not change.

Define `CertificationIdentity` in `verify.rs` beside `StructuralCertificate` so
verification owns the identity authority; certification, timing and equivalence
only borrow it.

- [ ] **Step 1: Write a failing identity-threading test**

Add a test beside `complete_certification_seals_structure_function_manifest_and_metrics` that constructs one `CertificationIdentity` from the NOT fixture, certifies through the new internal `certify_with_identity` entry and asserts every identity-bearing certificate and metric equals that identity. Never substitute a fake fingerprint for a structurally checked candidate.

- [ ] **Step 2: Run RED**

```powershell
cargo test --lib certification_identity_is_shared_by_every_certificate
```

Expected: compile failure because `CertificationIdentity` and `certify_with_identity` do not exist.

- [ ] **Step 3: Thread identity through existing authorities**

Create the identity once from `candidate.fingerprint()` and `library.revision_fingerprint()` at certification entry. Add internal variants that receive `&CertificationIdentity`:

- structural certification stores those values after running all existing checks;
- certification's internal timing derivation uses the sealed identity without serializing again; the public `RealisedTimingGraph::derive` keeps its current recomputation and mutation guard for unrelated callers;
- equivalence proof stores `identity.candidate.clone()` instead of serializing again;
- metrics use the same value.

Keep public wrappers that compute identity for unrelated callers. Do not weaken the timing graph's certificate mismatch check.

- [ ] **Step 4: Run GREEN and mutation guards**

```powershell
cargo test --lib certification_identity_is_shared_by_every_certificate
cargo test --lib compile::fragment_synth::timing_graph::tests::derivation_rejects_mutation_after_certification
cargo test --lib compile::equivalence::tests::wrong_assignment_and_selected_implementation_cannot_receive_a_certificate
```

Expected: all pass. The mutation test must still reject a candidate whose current fingerprint differs from the sealed identity.

- [ ] **Step 5: Commit identity reuse**

```powershell
git add -- src/compile/fragment_synth/verify.rs src/compile/fragment_synth/realise.rs src/compile/fragment_synth/timing_graph.rs src/compile/equivalence.rs src/compile/fragment_synth/certification.rs
git diff --cached --check
git commit -m "perf: reuse certified candidate identity"
```

### Task 5: Hoist immutable hierarchical artifacts

**Files:**
- Modify: `src/compile/fragment_synth/hierarchy_api.rs:148-226, 269-468, 764-794, 1025-1140`

**Interfaces:**
- Consumes: `LoweredHierarchy`, `CompiledBlock`, `PlannedParent`, `prunable_parent_routes` and `module_flattening`.
- Produces:

```rust
struct ModuleCompileContext {
    planning: Netlist,
    graph: InstanceGraph,
    flat: Netlist,
    paths: Vec<GatePath>,
}

struct RoutedParent {
    planned: PlannedParent,
    prunable_routes: BTreeSet<RouteId>,
}
```

`HierarchicalCandidate::planned` becomes `Arc<RoutedParent>`. The sidecar is always built from `planned.candidate.routes`; a changed placement creates a completely new `RoutedParent`.

- [ ] **Step 1: Write the failing context-reuse test**

Extend `unchanged_block_placements_reuse_the_incumbent_plan` to construct one `ModuleCompileContext`, compile baseline, evaluate and reject a moved proposal, then evaluate unchanged placements against the baseline. Assert:

```rust
assert!(Arc::ptr_eq(&baseline.planned, &reused.planned));
if let Ok(moved) = &moved {
    assert!(!Arc::ptr_eq(&baseline.planned, &moved.planned));
}
assert_eq!(fingerprint(&baseline), fingerprint(&reused));
```

The test passes the same context reference to every compile. Production types contain no callbacks, global counters or test-only cache behavior.

- [ ] **Step 2: Run RED**

```powershell
cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan
```

Expected: compile failure because the context and routed-parent types do not exist.

- [ ] **Step 3: Build one module context per compiled module**

`ModuleCompileContext::build` calls `parent_planning_graph` and `module_flattening` exactly once. `plan_parent` clones the stored graph only where the current planner requires ownership. `union_and_certify` borrows `context.flat` and `context.paths`; it does not call `module_flattening`.

The top context is created before the seed compile and captured by the proposal compiler. An intermediate module in `compile_blocks` gets one local context for its single compile. A flat single-module design keeps its existing fast path and does not allocate this context.

- [ ] **Step 4: Bind pruning data to the routed parent**

`RoutedParent::new(planned)` computes `prunable_parent_routes(&planned.candidate.routes)` once. `union_and_certify` uses the sidecar to retain realised parent route IDs. Never attach the sidecar to `HierarchicalCandidate` independently of the plan.

- [ ] **Step 5: Run GREEN and hierarchy regressions**

```powershell
cargo test --lib unchanged_block_placements_reuse_the_incumbent_plan
cargo test --lib compile::fragment_synth::hierarchy_api::tests -- --nocapture
```

Expected: all non-ignored hierarchy tests pass; deterministic budget traces and single/multi-thread fingerprints remain equal.

- [ ] **Step 6: Commit immutable context reuse**

```powershell
git add -- src/compile/fragment_synth/hierarchy_api.rs
git diff --cached --check
git commit -m "perf: reuse hierarchical compile invariants"
```

### Task 6: Benchmark, verify and decide retention

**Files:**
- Modify: `docs/superpowers/reports/2026-09-07-portable-performance-foundation.md`

**Interfaces:**
- Consumes: Layer 0 phase diagnostics and the completed Layer 1 implementation.
- Produces: a reproducible report with raw sample lists, medians, phase speedups, fingerprints, peak-memory observation and `KEEP` or `REVERT` per sub-change.

- [ ] **Step 1: Run focused release samples**

For each command, run three post-build samples before and after the implementation and preserve every elapsed/phase line:

```powershell
$env:REDA_PHASE_TIMING='1'
cargo test --release --lib unchanged_block_placements_reuse_the_incumbent_plan -- --nocapture
cargo test --release --lib input_seam_absorption_removes_the_child_refresh -- --ignored --nocapture
```

Expected: identical fingerprints and quality. Compare targeted phase medians, not Cargo build time.

- [ ] **Step 2: Run representative budget-zero checks**

```powershell
$env:REDA_EXTRA_CIRCUITS='ripple_adder8,multiplier4,alu8'
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture
```

Run only after focused samples; this command is intentionally long. Record each case separately and do not average away a regression.

- [ ] **Step 3: Run complete correctness verification serially**

```powershell
cargo test --lib
cargo test --test build_circuit_pins compile_hierarchical_preserves_the_checked_seven_segment_pin_contract -- --nocapture
cargo clippy --lib --tests
git diff --check
```

Expected: zero test failures; normal clippy may retain the repository's documented warnings but exits zero. Do not use `-D warnings` as this branch already has unrelated warning debt.

- [ ] **Step 4: Apply the retention gate**

For each sub-change, report:

```text
targeted phase before median / after median / speedup
end-to-end before median / after median / speedup
candidate fingerprint before / after
quality before / after
largest representative regression
KEEP or REVERT with reason
```

Revert any sub-change below 1.5x targeted-phase speedup or above 5% regression. Do not retain it because it is theoretically cleaner.

- [ ] **Step 5: Independent review**

Give a fresh `--model opus --effort high` Claude Code reviewer the spec, this
plan, complete diff and report. Require explicit findings for duplicate
authority removal, identity freshness, context-cache keys, pinned IO,
deterministic errors and benchmark arithmetic. Address Critical/Important
findings and rerun affected verification.

- [ ] **Step 6: Commit the final report**

```powershell
git add -- docs/superpowers/reports/2026-09-07-portable-performance-foundation.md
git diff --cached --check
git commit -m "docs: report portable performance foundation"
```

## Deferred Plans

Create these only after Task 6 records a `KEEP` decision and fresh profiling:

1. `2026-09-07-portable-parallel-certification.md` — deterministic exhaustive chunks and compilation-wide worker allocation.
2. `2026-09-07-portable-router-performance.md` — exact ancestry queries, reservation overlays and lookup indexes.
3. `2026-09-07-portable-incremental-simulator.md` — simulator template and tick-by-tick dirty-frontier differential oracle.
4. Proposal lookahead only if post-simulator profiling leaves at least 20% eligible wall time.
5. GPU prototype only if every evidence gate in spec section 9 passes.
