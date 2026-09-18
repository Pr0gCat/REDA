# Topology-Aware Seed v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fixed-ordinal sparse seed geometry with a deterministic topology-aware floorplan and route schedule so all six budget-zero acceptance cases certify and `and4` reaches at most 36 ticks and 944 non-air blocks.

**Architecture:** A new private `placement.rs` derives immutable DAG facts and a track-aware layered `SeedPlacementPlan` from `InstanceGraph`, selected topology, physical port geometry, and fixed pins. `seed.rs` materialises that plan, constructs a separate canonical `RouteSchedule`, and performs bounded fresh-candidate repairs while preserving the existing router, verifier, certifier, and fragment optimiser.

**Tech Stack:** Rust 2021, ordered `BTreeMap`/`BTreeSet` state, existing REDA physical router/simulator/verifier, Cargo unit and release acceptance tests.

**Spec:** `docs/superpowers/specs/2026-09-01-topology-aware-seed-v2.md`

## Global Constraints

- Do not modify the physical A* router or increase the checked `RouterLimits` values of 262144 node expansions and 262144 queue entries.
- Seed construction is deterministic, evaluation-budget independent, legacy-free, and bounded only by existing checked seed caps.
- `PortPin.toward`, `PortPin::handover`, and `PortPin::net_cell` remain the pin geometry authority; all 11 checked pinned seven-segment `(Anchor, toward)` pairs remain byte-identical.
- Pinned caller cells remain air and pinned boundaries never enter an internal placement candidate set.
- Every route attempt starts from a fresh candidate; failed attempts cannot leak placements, routes, reservations, worlds, timing graphs, or fingerprints.
- Public `SynthesisInput` and `compile_fragment_synth` signatures remain unchanged.
- The legacy front doors remain unchanged unless the parent replacement gate passes in full.
- Production behavior changes follow strict RED, GREEN, REFACTOR cycles with real behavior assertions.

---

### Task 1: Add pure instance-DAG and structural timing analysis

**Files:**
- Create: `src/compile/fragment_synth/placement.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Test: `src/compile/fragment_synth/placement.rs` (`tests` module)

**Interfaces:**
- Consumes: `InstanceGraph`, selected `ExpandedInstance` topology, typed `PhysicalDriver` and `PhysicalSink` assignments.
- Produces: `analyse_instance_dag(&InstanceGraph) -> Result<SeedPlacementAnalysis, SeedPlacementError>`.
- Produces ordered `NodeFacts { predecessors, successors, forward_level, reverse_level, head_ticks, tail_ticks }` and `EdgeFacts { source, sink, structural_slack_ticks }`.

- [ ] **Step 1: Write a reversed-declaration dependency test**

Construct a netlist whose consumer is gate index 0 and whose producer is gate index 1, then build `InstanceGraph::new` or `with_variants`. Assert literal facts:

```rust
let facts = analyse_instance_dag(&graph).unwrap();
assert_eq!(facts.nodes[&InstanceId(1)].forward_level, 0);
assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 1);
assert_eq!(facts.order, [InstanceId(1), InstanceId(0)]);
```

The production mutation this catches is replacing dependency order with `logical_gate.0` order.

- [ ] **Step 2: Add fanout, reverse-level, slack, duplicate, and cycle tests**

Use literal expected levels and typed IDs. Require duplicate instances to appear independently. Build a malformed cyclic `InstanceGraph` directly and require:

```rust
assert!(matches!(
    analyse_instance_dag(&cycle),
    Err(SeedPlacementError::DependencyCycle { .. })
));
```

The tests must not call the analysis implementation to derive expected values.

- [ ] **Step 3: Run the focused test and verify RED**

```powershell
cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
```

Expected: compilation fails because `placement` and `analyse_instance_dag` do not exist.

- [ ] **Step 4: Implement the minimal immutable analysis**

Use this shape and ordered collections:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SeedPlacementAnalysis {
    pub order: Vec<InstanceId>,
    pub nodes: BTreeMap<InstanceId, NodeFacts>,
    pub edges: Vec<EdgeFacts>,
    pub critical_delay_ticks: u64,
}

pub(crate) fn analyse_instance_dag(
    graph: &InstanceGraph,
) -> Result<SeedPlacementAnalysis, SeedPlacementError>;
```

Use Kahn topological ordering with `InstanceId` as the ready-set tie-break. Compute forward and reverse max-plus passes. Selected topology primitive costs supply cell-only delays; pre-route wire delay is exactly zero.

- [ ] **Step 5: Run GREEN and existing graph tests**

```powershell
cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
cargo test --lib compile::fragment_synth::instance_graph::tests -- --nocapture
```

Expected: all tests pass with no ignored test added.

- [ ] **Step 6: Format only touched Rust files and commit**

```powershell
rustfmt --edition 2021 src/compile/fragment_synth/placement.rs src/compile/fragment_synth/mod.rs
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/mod.rs
git commit -m "feat(synthesis): analyse seed placement topology"
```

---

### Task 2: Build the track-aware layered placement plan

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs`
- Modify: `src/compile/fragment_synth/seed.rs` only to expose existing physical-geometry helpers with sibling visibility when needed
- Test: `src/compile/fragment_synth/placement.rs` (`tests` module)

**Interfaces:**
- Consumes: Task 1 `SeedPlacementAnalysis`, resolved typed pin contracts, `physical::variants`, `ValidatedTopology`, and `EmbeddingHint`.
- Produces: `TopologyAwareSeedPlacer`, `SeedPlacementRequest`, `SeedPlacementPlan`, `PreferredInstancePose`, automatic input/output homes, and a canonical plan fingerprint.
- The plan contains preferred origins and facings only; it does not own routes, reservations, or final primitive coordinates.

- [ ] **Step 1: Write frame and interval-track RED tests**

Cover four literal frame cases: no pins -> East; checked input/output geometry -> North; input-only majority; output-only majority. Add interval tests proving overlapping lifetimes get distinct tracks and disjoint lifetimes reuse a track. Compare complete repeated plans:

```rust
assert_eq!(placer.plan(request()).unwrap(), placer.plan(request()).unwrap());
```

The production mutations caught are fixed world-X placement and one-track-per-net growth.

- [ ] **Step 2: Write actual physical-port facing tests**

For one torch and one repeater fixture, assert the chosen facing minimizes hand-derived source-output and target-input port distances. The expectation must name literal `CellFacing` values and literal local/world anchors. Add an embedding-hint fixture where removing the hint penalty changes the expected pose.

- [ ] **Step 3: Verify RED**

```powershell
cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
```

Expected: new frame, track, and facing tests fail because the planner types are absent.

- [ ] **Step 4: Implement frame, envelopes, interval colouring, and lane order**

Implement these exact responsibilities:

```rust
pub(crate) trait SeedPlacer {
    fn plan(
        &self,
        request: SeedPlacementRequest<'_>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TopologyAwareSeedPlacer;
```

Derive macro envelopes from selected `PhysicalVariant` blocks and ports. Assign columns by forward level, tracks by stable interval colouring, instances by incident-track weighted median, then run one forward and one reverse stable barycentric sweep. Enumerate facing in NESW order and score actual port coordinates plus topology/embedding penalties. Apply a deterministic overlap legalizer to preferred macro origins.

- [ ] **Step 5: Verify determinism and formatting**

```powershell
cargo test --lib compile::fragment_synth::placement::tests -- --nocapture
rustfmt --edition 2021 src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs
```

Expected: all placement tests pass twice in the same process with equal complete plans and fingerprints.

- [ ] **Step 6: Commit**

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): plan topology-aware seed geometry"
```

---

### Task 3: Wire Seed v2 into materialisation and preserve pinned IO

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs`
- Modify: `src/compile/fragment_synth/services.rs`
- Modify: `src/compile/fragment_synth/api.rs`
- Modify: `tests/build_circuit_pins.rs`
- Test: `src/compile/fragment_synth/seed.rs` (`tests` module)

**Interfaces:**
- Consumes: Task 2 `SeedPlacer` and `SeedPlacementPlan`.
- `SeedServices` gains `placer: &'a dyn SeedPlacer`; all production and test constructors provide it explicitly.
- `place_boundaries` consumes plan-owned automatic homes only for unpinned ports.
- `place_instances` consumes planned instance poses and then applies existing `InstancePlacementOverride` values relative to the preferred origin.
- `CaseDescriptor` gains `placement_revision: Fingerprint` with a fixed `topology-aware-seed-v2` revision provider.

- [ ] **Step 1: Write the injected-plan RED test**

Define a fake placer in the existing `seed.rs` test module. Return literal preferred origins/facings and automatic boundary homes. Assert the materialised primitive placements and compatibility input/output positions use those values. Also assert the fake placer is called exactly once.

- [ ] **Step 2: Write the 11-pin invariant RED test**

Use the checked pinned seven-segment fixture and literal pairs from the benchmark:

```text
outputs: (76,1,24,N), (84,1,32,E), (84,1,48,E),
         (76,1,56,S), (68,1,48,W), (68,1,32,W), (76,1,40,W)
inputs:  (76,1,120,N), (88,1,120,N), (100,1,120,N), (112,1,120,N)
```

Require unchanged `PortPlacements`, caller air, exact handover/net-cell values, and every internal placement on the inward side of the input boundary. The test catches moving pins or retaining the old east-of-max-X policy.

- [ ] **Step 3: Write the case-fingerprint revision RED test**

Require the same netlist/config/library with different literal placement revisions to produce different case fingerprints. Keep candidate fingerprint semantics unchanged.

- [ ] **Step 4: Verify RED**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo test --lib compile::fragment_synth::api::tests -- --nocapture
cargo test --test build_circuit_pins -- --nocapture
```

Expected: injected placer and placement revision tests fail before wiring.

- [ ] **Step 5: Implement the wiring without changing public APIs**

Instantiate `TopologyAwareSeedPlacer` in the production service bundle. Delete fixed `INSTANCE_BASE_X`, `INSTANCE_X_PITCH`, and `INSTANCE_Z_PITCH` use from default instance placement; retain only constants still required by physical primitive legalization. Do not retain a runtime policy switch to Seed v1.

- [ ] **Step 6: Run pin, handover, architecture, and seed GREEN tests**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo test --lib compile::fragment_synth::api::tests -- --nocapture
cargo test --test build_circuit_pins -- --nocapture
cargo test --test terminal_handover -- --nocapture
cargo test --test fragment_synth_architecture -- --nocapture
```

Expected: all pass; no legacy generation counter is incremented.

- [ ] **Step 7: Format and commit**

```powershell
rustfmt --edition 2021 src/compile/fragment_synth/seed.rs src/compile/fragment_synth/services.rs src/compile/fragment_synth/api.rs tests/build_circuit_pins.rs
git add src/compile/fragment_synth/seed.rs src/compile/fragment_synth/services.rs src/compile/fragment_synth/api.rs tests/build_circuit_pins.rs
git commit -m "feat(synthesis): materialise topology-aware seeds"
```

---

### Task 4: Add canonical route scheduling and typed failure evidence

**Files:**
- Create: `src/compile/fragment_synth/route_schedule.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/fragment_synth/seed.rs`
- Test: `src/compile/fragment_synth/route_schedule.rs` (`tests` module)
- Test: `src/compile/fragment_synth/seed.rs` (`tests` module)

**Interfaces:**
- Consumes: placement analysis, placed source/target geometry, and grouped `PendingTarget` obligations.
- Produces: `RouteSchedule { routes: Vec<ScheduledRoute> }`, where each route records source identity and ordered typed targets before `RouteId` assignment.
- Produces `SeedRoutingFailure { scheduled_index, route, source, sink, category, limit, work_used, plan_fingerprint, source_at, sink_at }`; `limit` and `work_used` are `Option<u64>` and are `None` for `Refused`/physical-invariant failures.

- [ ] **Step 1: Write recording-router schedule RED tests**

Use a real `InstanceGraph` plus recording router. Require the literal priority
ordering: pinned boundary escape, lower slack, higher fanout, longer span,
source ID. Within a fanout tree require lower slack, then decreasing forward
distance, then `PendingTarget` key. Add literal controls for the measured
failures: `full_adder` input-a schedules `g8.in0` before `g0.in0`; segment route
source `g0` schedules `g35`, then `g32`, then `g4`; the pinned input-d2 tree
schedules `g17` before its nearer sinks. Reverse insertion order and require the
same complete schedule.

- [ ] **Step 2: Write typed failure RED tests**

Inject one `RouterLimitExceeded` and one `RingClosure`. Require exact source/sink IDs, anchors, category, optional limit/work, and placement-plan fingerprint in `SeedError`. Assert the candidate and reservation recorder remain at their pre-attempt fingerprints after failure.

- [ ] **Step 3: Verify RED**

```powershell
cargo test --lib compile::fragment_synth::route_schedule::tests -- --nocapture
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
```

Expected: compilation or assertions fail because current `route_all` sorts only by earliest target rank and flattens failure context.

- [ ] **Step 4: Implement the pure schedule and adapter**

Build the complete schedule before assigning route IDs. Reserve every terminal before routing. `seed.rs` adapts each scheduled route one-to-one into the unchanged `PhysicalRouter::route` request. Do not move scheduling policy into `routing.rs`.

- [ ] **Step 5: Run GREEN and router regressions**

```powershell
cargo test --lib compile::fragment_synth::route_schedule::tests -- --nocapture
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo test --lib compile::routing::tests -- --nocapture
cargo test --test channel_safety -- --nocapture
```

Expected: all pass under unchanged router caps.

- [ ] **Step 6: Format and commit**

```powershell
rustfmt --edition 2021 src/compile/fragment_synth/route_schedule.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/seed.rs
git add src/compile/fragment_synth/route_schedule.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): schedule topology-aware seed routes"
```

---

### Task 5: Add bounded fresh-candidate layout repair

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs`
- Modify: `src/compile/fragment_synth/route_schedule.rs`
- Modify: `src/compile/fragment_synth/seed.rs`
- Modify: `src/compile/fragment_synth/fragment.rs` (map `SeedExhausted` only)
- Test: `src/compile/fragment_synth/seed.rs` (`tests` module)

**Interfaces:**
- Produces ordered `LayoutRepair` values: `ExclusiveGuardedTrack`, `EarlyTreeSinkAndEscape`, and `SeparateOwners`.
- `TopologyAwareSeedPlacer::plan_with_repairs(request, &[LayoutRepair])` returns a complete new plan.
- Seed attempt state contains only canonical repairs and attempts used; every attempt reconstructs `ExpandedPhysicalCandidate`, occupied state, reservations, routes, world, and certification state from scratch.

- [ ] **Step 1: Write one RED test per repair class**

Use deterministic failing routers that fail the first request and accept only when their required geometric/schedule property changes. Require one repair, one fresh rebuild, and certification. Mutate the rebuild to reuse the failed candidate and ensure the fingerprint assertion catches it.

- [ ] **Step 2: Write cap and repetition RED tests**

Set `max_seed_backtracks` to literal small values. Require exact attempts used, final typed refusal, and `SeedExhausted` when the same canonical repair would repeat. Require router limits and pin fingerprints to remain constant across all attempts.

- [ ] **Step 3: Verify RED**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
```

Expected: repair tests fail because current seed makes exactly one fixed layout attempt.

- [ ] **Step 4: Implement the minimal monotonic repair loop**

Map failures exactly as specified, without pretending all categories have one
root cause:

```rust
fanout && !promoted => LayoutRepair::EarlyTreeSinkAndEscape { source, sink };
otherwise => LayoutRepair::SeparateOwners { source_owner, sink_owner };
```

Canonicalize and deduplicate repairs before rebuilding. A duplicate repair terminates with `SeedExhausted`. Certification failure is returned directly and is not converted into geometry guessing.

Treat only typed verifier `CrossRouteConnectivity` and `CrossRouteCoupling` as
repairable by adding `ExclusiveGuardedTrack` for the later movable route source. Return every other
verification failure directly. `SeedExhausted` records attempts used and the
final typed router or cross-route refusal, and maps to the existing seed
backtrack-cap terminal reason.

- [ ] **Step 5: Run focused GREEN and the six-case budget-zero harness**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output $env:TEMP/reda-seed-v2-correctness.json --shipping-source src/compile/fragment_synth/shipping_config.rs --shuffle-seed 0x5245444120260831
```

Expected milestone: all six budget-zero cases have `compiled_and_certified: true`; no case error contains `PhysicalInvariant`, `QueueEntries`, or `SeedExhausted`. The overall replacement gate may still fail quality and must not generate `shipping_config.rs`.

- [ ] **Step 6: Format and commit**

```powershell
rustfmt --edition 2021 src/compile/fragment_synth/placement.rs src/compile/fragment_synth/route_schedule.rs src/compile/fragment_synth/seed.rs
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/route_schedule.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): repair failed seed floorplans"
```

---

### Task 6: Meet seed-quality thresholds and record acceptance evidence

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs` only for measured scoring weights or spacing constants
- Modify: `src/compile/fragment_synth/route_schedule.rs` only if measured route order is the proven cause
- Modify: `tests/fragment_synth_acceptance.rs`
- Modify: `tests/fixtures/fragment_synth_shipping.json`
- Create: `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/acceptance-report.md`

**Interfaces:**
- Consumes: certified Task 5 seed and unchanged acceptance evaluator.
- Produces checked machine-readable six-case report plus a concise evidence report.
- Does not create shipping configuration unless the complete parent replacement gate passes.

- [ ] **Step 1: Add a non-ignored threshold RED test**

Evaluate budget-zero `and4` with the real benchmark evaluator. Assert:

```rust
assert!(case.compiled_and_certified);
assert!(case.measured.as_ref().unwrap().max_observed_settle_game_ticks_on_manifest.unwrap() <= 36);
assert!(case.measured.as_ref().unwrap().physical.as_ref().unwrap().non_air_blocks <= 944);
```

The production mutations caught are widening all level/track gaps or restoring fixed ordinal placement.

- [ ] **Step 2: Verify RED or record already-green evidence**

```powershell
cargo test --release --test fragment_synth_acceptance topology_aware_seed_v2_and4_quality -- --nocapture
```

Expected: RED if Task 5 correctness geometry still exceeds either threshold. If already green, record the exact measured ticks/blocks and do not tune further.

- [ ] **Step 3: Tune one measured variable at a time**

For each RED run, change only one of: forward channel gap, lateral track pitch, critical-edge weight, or fanout guard width. Record before/after `and4` ticks, blocks, and whether all six cases remain certified. Revert any change that improves `and4` while breaking correctness. Do not change router or certification caps.

- [ ] **Step 4: Run focused and complete release acceptance**

```powershell
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output tests/fixtures/fragment_synth_shipping.json --shipping-source src/compile/fragment_synth/shipping_config.rs --shuffle-seed 0x5245444120260831
```

Expected: six budget-zero cases certify and `and4 <= 36 ticks / <= 944 blocks`. If the full replacement gate remains false, the JSON must name only real quality failures, `repeatability_executed` must accurately describe executed work, and `shipping_config.rs` must remain absent.

- [ ] **Step 5: Run complete verification**

```powershell
cargo test --release --lib compile::fragment_synth -- --nocapture
cargo test --release --test build_circuit_pins -- --nocapture
cargo test --release --test terminal_handover -- --nocapture
cargo test --release --test fragment_synth_architecture -- --nocapture
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo clippy --all-targets --all-features -- -D warnings -A clippy::clone-on-copy -A clippy::field-reassign-with-default
git diff --check
git status --short
```

Expected: all commands exit 0; only intended report/source changes are present before commit.

- [ ] **Step 6: Write evidence report and commit**

The report lists the baseline command, six per-case certification outcomes, exact ticks/blocks, pinned manifest fingerprint, test commands, and whether the full replacement gate passed. Then:

```powershell
git add src/compile/fragment_synth/placement.rs src/compile/fragment_synth/route_schedule.rs tests/fragment_synth_acceptance.rs tests/fixtures/fragment_synth_shipping.json .superpowers/sdd/2026-09-01-topology-aware-seed-v2/acceptance-report.md
git commit -m "test(synthesis): record topology-aware seed acceptance"
```

---

## Final Review Gates

- Every task receives a task-scoped spec and quality review before the next task.
- The final reviewer checks the full range from `da9922e` to HEAD against this plan and the Seed v2 spec.
- Re-run fresh verification after final-review fixes.
- Mark the goal complete only when all six budget-zero cases certify, pinned geometry is unchanged, and `and4 <= 36 ticks / <= 944 blocks` is evidenced by the checked harness.
