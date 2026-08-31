# Timing-Directed Fragment Synthesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a deterministic, topology-aware fragment synthesiser that always returns a certified sparse seed, spends larger evaluation or time budgets on a prefix-compatible optimisation stream, and ranks candidates first by observed redstone latency and then by emitted size.

**Architecture:** Add a new `compile::fragment_synth` subsystem beside the current generators. It owns typed physical identities, pure cell-topology expansion, a complete candidate model, sparse seed construction, primitive-level timing, certification, and fragment search. Existing cell-library data, routing physics, emitter, verifier rules, simulator, and pinned-IO contract remain authoritative but are reached through lossless typed adapters. The shipping `compile` path stays unchanged until the recorded replacement gate passes.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, SHA-256 canonical fingerprints, REDA cell topology and physical variants, existing redstone simulator and verifier rules, Cargo unit/integration tests.

**Spec:** `docs/superpowers/specs/2026-08-31-timing-directed-fragment-synthesis.md`

## Global Constraints

- This milestone accepts lowered, combinational netlists only. Return `UnsupportedStatefulTopology` before `InstanceGraph` construction and preserve the existing named errors for combinational cycles and undriven signals.
- The new generator must not call `compile_legacy`, `compile_planned`, `compile_grown`, `seed_from_legacy`, `plan_from_netlist`, spring relaxation, or the old optimiser after Task 8's independent seed exists. The legacy adapter introduced in Tasks 3-4 remains migration/differential-test code only; Task 8 only verifies that boundary with a call spy.
- Do not migrate `PrimitiveId` into `relax/build.rs` or `relax/snap.rs`. Those files remain part of the legacy oracle until deletion; the new `SparseSeedBuilder` owns typed primitive placement directly. The one-to-one migration adapter observes a completed legacy seed and never makes legacy relaxation part of new candidate construction.
- Every externally visible candidate is complete, independently re-instantiated from the immutable library, physically verified, functionally certified, and transition-measured. Rejected transaction state never mutates the accepted parent.
- Logical ownership, electrical endpoint identity, and physical coordinates are separate fields. No map key may infer one from another.
- Every topology primitive and internal edge is materialised, routed, timed, observed when delayed, and structurally verified. `Template::output = None` creates a junction, never a fictional gate body.
- `ImplementationKey::Merge { isolation_mask }` is the one parameterised implementation of bare, mixed, and fully isolated merges. The mask and expanded-topology fingerprint participate in candidate identity.
- Pinned cells remain caller-owned air in the emitted world. Input travel is `at + toward`; output travel is `at - toward`; only that handover neighbour may carry the pin signal.
- At every caller-owned input or output pin, external high means signal strength `> 0`; neither certification nor fixtures may require strength 15.
- Static timing counts simulator game ticks from actual primitive and route-owned repeaters. Dust geometry has zero timing delay. Pinned input and output repeaters are each charged exactly once.
- Evaluation budgets are deterministic and prefix-compatible. Time budgets run the same proposal stream and inspect the deadline only before starting a whole transaction.
- `World::size()` is storage allocation, not physical size. Shared metrics count non-air blocks and the occupied bounding box.
- Do not change production front doors or remove a legacy path until Task 13's clean-checkout replacement gate passes. A failed gate leaves the experiment explicit and isolated.
- `Anchor`, typed routing, physical emission, and physical verification live in durable modules outside `planner.rs`; old and new generators are adapters over those modules. Legacy deletion removes placement/search policies and fallback selection only, never shared routing physics, emission, or verifier rules.

---

### Task 0: Add durable coordinates, canonical fingerprints, metrics, and revision providers

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/geometry.rs`
- Modify: `src/compile/planner.rs`
- Modify: `src/compile/topology.rs`
- Create: `src/compile/metrics.rs`
- Create: `src/compile/revisions.rs`
- Test: `src/compile/metrics.rs` (`tests` module)
- Test: `src/compile/revisions.rs` (`tests` module)

**Interfaces:**
- Produces durable `geometry::Anchor`, `Fingerprint`, `Ratio`, `PhysicalMetrics`, `canonical_fingerprint`, `physical_metrics`, `cell_library_revision`, `simulator_revision`, and `physical_verifier_revision` before Task 1 captures any baseline.
- Consumes only deterministic byte/string encodings and `World`; it has no generator dependency.

- [ ] **Step 1: Write failing metric and fingerprint tests**

Add tests with a padded `World::new(20, 8, 20)` containing three non-air blocks at `(2,1,3)`, `(4,1,3)`, and `(4,2,5)`. Require:

```rust
assert_eq!(metrics.non_air_blocks, 3);
assert_eq!(metrics.occupied_min, Anchor { x: 2, y: 1, z: 3 });
assert_eq!(metrics.occupied_max, Anchor { x: 4, y: 2, z: 5 });
assert_eq!(metrics.occupied_volume, 18);
assert_eq!(metrics.blocks_per_lowered_gate, Ratio::new(3, 2));
assert_eq!(physical_metrics(&padded, 2), physical_metrics(&tightly_sized, 2));
assert_eq!(canonical_fingerprint(b"reda"), canonical_fingerprint(b"reda"));
assert_ne!(canonical_fingerprint(b"reda"), canonical_fingerprint(b"REDA"));
```

Also require an all-air world to report zero blocks, volume zero, and both bounds as `None`; this avoids inventing a coordinate for an empty circuit. Add a compatibility test proving `planner::Anchor` is exactly the re-exported `geometry::Anchor`, so no durable candidate or metric type depends on deletable planner policy.

- [ ] **Step 2: Run the focused test and verify RED**

```powershell
cargo test --lib compile::metrics::tests -- --nocapture
```

Expected: compilation fails because the metric, durable coordinate, and revision-provider APIs do not exist.

- [ ] **Step 3: Implement exact metric and fingerprint types**

Add `sha2 = "0.10"`. Define serialisable ordered values:

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Fingerprint(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio { pub numerator: u64, pub denominator: u64 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalMetrics {
    pub non_air_blocks: u64,
    pub occupied_min: Option<Anchor>,
    pub occupied_max: Option<Anchor>,
    pub occupied_volume: u64,
    pub blocks_per_lowered_gate: Ratio,
}
```

Move `Anchor` from `planner.rs` to `compile::geometry`, preserving its derives and fields, and use `pub use crate::compile::geometry::Anchor;` from `planner.rs` for source compatibility. `canonical_fingerprint` returns lowercase SHA-256 hex. `physical_metrics` scans `World::cells()` in YZX order, ignores `BlockKind::Air`, uses inclusive extents, and uses denominator `max(lowered_gate_count, 1)`.

- [ ] **Step 4: Implement deterministic authority revision providers**

In `revisions.rs`, define canonical, serialisable revision descriptors rather than reading Git state or wall-clock data. `cell_library_revision(&Library)` hashes every gate kind, stable entry ordinal, template node/primitive, input mapping, internal edge, output, and embedding hint in registration order. `simulator_revision()` hashes an explicit descriptor containing supported/unsupported component kinds, component delay constants, tick-priority ordering, propagation rule version, and the `power > 0` wire-observation policy. `physical_verifier_revision()` hashes the ordered verifier rule IDs and semantic versions for collision, coupling, connectivity, torch/merge structure, signal strength, repeater direction, terminal style, and pin handover/halo checks. The owning module must update its descriptor entry in the same commit as a semantic rule change.

Tests require repeat calls to be equal and non-empty, map insertion order not to matter, and a one-field mutation of each test descriptor to change its fingerprint. These production providers, not literals in the baseline tool, are the only source of Task 1's three revision fields.

- [ ] **Step 5: Run focused, revision, geometry, and world regressions**

```powershell
cargo test --lib compile::metrics::tests -- --nocapture
cargo test --lib compile::revisions::tests -- --nocapture
cargo test --lib compile::geometry::tests -- --nocapture
cargo test --lib redstone::world -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 0**

```powershell
git add Cargo.toml Cargo.lock src/compile/mod.rs src/compile/geometry.rs src/compile/planner.rs src/compile/topology.rs src/compile/metrics.rs src/compile/revisions.rs
git commit -m "feat(compile): add canonical physical metrics"
```

---

### Task 1: Record the immutable legacy benchmark baseline

**Files:**
- Create: `src/compile/fragment_synth/mod.rs`
- Create: `src/compile/fragment_synth/benchmark.rs`
- Create: `src/compile/fragment_synth/manifest.rs`
- Create: `src/bin/fragment_baseline.rs`
- Create: `tests/fixtures/fragment_synth_baseline.json`
- Create: `tests/fragment_synth_baseline.rs`
- Modify: `src/compile/mod.rs`

**Interfaces:**
- Produces `TransitionManifest`, `BenchmarkBaseline`, `BenchmarkCase`, and one evaluator used unchanged for legacy and new worlds.
- Consumes Task 0's production revision providers and records build/profile revisions, input hashes, fixed transition manifest hash, world fingerprint, physical metrics, and observed settle ticks. The capture command has no revision-string override.

- [ ] **Step 1: Write the failing in-memory baseline-schema test**

In `benchmark.rs`'s test module, deserialize a literal minimal JSON value and assert the ordered case names are exactly:

```rust
[
    "and4",
    "verilog:and4",
    "full_adder",
    "segment_a",
    "seven_segment",
    "pinned:verilog:seven_segment",
]
```

For every certified case require non-empty baseline commit/world/netlist/manifest fingerprints, positive non-air count, a non-zero transition count, and an explicit `certified: bool`. Numeric baseline fields are `Option` so a case the legacy front door cannot certify is recorded as new coverage rather than silently omitted.

- [ ] **Step 2: Run the schema test and verify RED**

```powershell
cargo test --lib compile::fragment_synth::benchmark::tests -- --nocapture
```

Expected: compilation fails because `BenchmarkBaseline`, `BenchmarkCase`, and their literal-schema deserialisation do not exist; this in-memory test does not open a fixture.

- [ ] **Step 3: Implement the shared acceptance evaluator**

First implement the immutable transition-manifest builder in `manifest.rs`: at up to four inputs it emits every ordered pair of distinct vectors; above four inputs it emits all-zero, all-one, one-hot, one-cold, and every single-bit toggle among those vectors. Its canonical order and hash depend only on ordered input ports and manifest policy, never on a generated candidate. Task 7 will reuse this exact type for certification rather than rebuilding the workload.

Define:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkCase {
    pub name: String,
    pub lowered_netlist_hash: Fingerprint,
    pub pin_manifest_hash: Fingerprint,
    pub transition_manifest_hash: Fingerprint,
    pub generated_world_fingerprint: Option<Fingerprint>,
    pub certified: bool,
    pub physical: Option<PhysicalMetrics>,
    pub max_observed_settle_game_ticks_on_manifest: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkBaseline {
    pub baseline_commit: String,
    pub build_profile: String,
    pub cargo_features: Vec<String>,
    pub cell_library_revision: Fingerprint,
    pub simulator_revision: Fingerprint,
    pub verifier_revision: Fingerprint,
    pub cases: Vec<BenchmarkCase>,
}
```

The evaluator owns caller fixtures, creates a fresh simulator for every transition, uses a 2,048-game-tick cap, checks truth outputs, and canonicalises the emitted world by size, YZX coordinates, block kind, facing, power, lit, delay, and face.

- [ ] **Step 4: Commit the evaluator before capturing data**

Run its unit/schema tests with a minimal temporary fixture, then commit the evaluator and manifest policy without the immutable corpus data:

```powershell
git add src/compile/mod.rs src/compile/fragment_synth/benchmark.rs src/compile/fragment_synth/manifest.rs src/compile/fragment_synth/mod.rs src/bin/fragment_baseline.rs
git commit -m "feat(synthesis): add the shared acceptance evaluator"
```

- [ ] **Step 5: Generate and inspect the baseline from that clean commit**

The binary must refuse to overwrite an existing path unless `--replace` is passed and must print one summary row per case.

```powershell
git status --short
cargo run --release --bin fragment_baseline -- --output tests/fixtures/fragment_synth_baseline.json
git diff -- tests/fixtures/fragment_synth_baseline.json
```

Expected: `git status --short` is empty before capture, and the result is one complete deterministic JSON manifest whose `baseline_commit` is the evaluator commit; no historical metric is typed by hand.

- [ ] **Step 6: Add immutability and repeatability checks**

Create `tests/fragment_synth_baseline.rs`, load the checked fixture, and require the same schema/case order as the in-memory test. Run the evaluator twice in fresh processes to temporary output paths and byte-compare the JSON after excluding an optional human-readable elapsed-time field. The checked fixture test must fail if case order, revision fields, or hashes disappear.

```powershell
cargo test --test fragment_synth_baseline -- --nocapture
```

Expected: pass.

- [ ] **Step 7: Commit the immutable baseline data**

```powershell
git add tests/fragment_synth_baseline.rs tests/fixtures/fragment_synth_baseline.json
git commit -m "test(synthesis): record immutable legacy baseline"
```

---

### Task 2: Introduce typed instance/observation identity and pure topology instantiation

**Files:**
- Create: `src/compile/fragment_synth/identity.rs`
- Create: `src/compile/fragment_synth/topology.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/topology.rs`
- Modify: `src/compile/primitive_graph.rs`
- Test: `src/compile/fragment_synth/topology.rs` (`tests` module)

**Interfaces:**
- Produces `GateIndex`, `PortId`, `LibraryEntryId`, `InstanceId`, `TopologyNodeId`, `PrimitiveId`, `ConnectionId`, `RouteId`, `RoutedSinkId`, `PhysicalEndpointId`, `ObservationId`, `ObservationSite`, `ImplementationKey`, `ExpandedInstance`, and `ValidatedTopology`.
- Consumes immutable `topology::Library`, `LibraryEntry`, `Template`, `TemplateNode`, and `Primitive`.

- [ ] **Step 1: Write exact expansion tests before the API exists**

Cover five forms directly through `instantiate`, not through `primitive_graph::expand`:

1. one-node NOR: one torch, one external connection per input, `OutputSpec::Primitive`;
2. two-node BUF: two torches, one external and one internal `ConnectionId`, output on `SecondTorch`;
3. bare merge: no primitive, one external landing per input, a junction with ordered landing contributors;
4. mask `0b01`: one isolating repeater only on input 0 plus one bare landing contributor;
5. all-set merge: one repeater per input and a junction of their primitive outputs.

Also assert dense deterministic node IDs, stable connection ordering, fingerprint repeatability, and named errors for an unknown library entry, illegal mask width, unresolved template input/output, duplicate template role, and `GateKind::Dff`.

- [ ] **Step 2: Run the focused tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::topology::tests -- --nocapture
```

Expected: compilation fails because typed identity and instantiation do not exist.

- [ ] **Step 3: Add stable library IDs without moving policy into the library**

Define `LibraryEntryId { kind: GateKind, ordinal: u16 }`; add `Library::entry_id_at`, `Library::entry`, and a canonical `Library::revision_fingerprint`. Keep `entry_cost` as an estimate only. Correct the stale `Template` documentation that claims every shipped entry has one node and no internal edges.

- [ ] **Step 4: Implement pure validated expansion**

Use these core signatures:

```rust
pub fn instantiate(
    library: &Library,
    gate: &Gate,
    instance: InstanceId,
    implementation: &ImplementationKey,
) -> Result<ExpandedInstance, TopologyError>;

pub struct ValidatedTopology {
    pub fingerprint: Fingerprint,
    pub primitives: Vec<PrimitiveSpec>,
    pub connections: Vec<ConnectionSpec>,
    pub output: OutputSpec,
}
```

Assign IDs only from instance plus ordered template role/edge/input indexes. Build the fingerprint from the complete canonical expanded value after validation. `EmbeddingHint` is retained as ordered metadata and never changes validity.

Define observation identity in `identity.rs` now, before Task 3 stores it:

```rust
pub enum ObservationId {
    PrimaryInput(PortId),
    PrimitiveOutput(PrimitiveId),
    InstanceOutput(InstanceId),
    JunctionOutput(InstanceId),
    DeclaredOutput(PortId),
}

pub struct ObservationSite {
    pub id: ObservationId,
    pub at: Anchor,
    pub logical_owner: Option<InstanceId>,
    pub display_label: Option<String>,
}
```

All identity variants derive deterministic ordering, hashing, and serialisation. `ObservationSite::id` is authoritative; position and display label are payload and never key or merge observations. Task 6 adds simulator behavior and the one-to-one mapping from observable `TimingNodeId` variants to this already-stable family.

- [ ] **Step 5: Run topology and old primitive-graph regressions**

Move the existing merge branch-sharing decision into one pure `merge_isolation_mask(lowered, gate_index)` query used by both the new instantiator and legacy `primitive_graph::expand_with_selection`. Add parity tests for all-bare, mixed, and all-isolated masks; keep the old `PrimitiveGraph` result type and consumers unchanged.

```powershell
cargo test --lib compile::fragment_synth::topology::tests -- --nocapture
cargo test --lib compile::primitive_graph -- --nocapture
cargo test --test primitive_graph_equivalence -- --nocapture
```

Expected: all pass; the old whole-netlist expansion remains unchanged.

- [ ] **Step 6: Commit Task 2**

```powershell
git add src/compile/mod.rs src/compile/topology.rs src/compile/primitive_graph.rs src/compile/fragment_synth
git commit -m "feat(synthesis): add validated topology instances"
```

---

### Task 3: Build the one-to-one `InstanceGraph` and complete candidate state

**Files:**
- Create: `src/compile/fragment_synth/instance_graph.rs`
- Create: `src/compile/fragment_synth/candidate.rs`
- Create: `src/compile/fragment_synth/legacy_adapter.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/fragment_synth/instance_graph.rs` and `candidate.rs` (`tests` modules)
- Test: `tests/compile_end_to_end.rs`

**Interfaces:**
- Produces ordered `Instance`, `SinkAssignment`, `InstanceGraph`, `PhysicalState`, `ExpandedPhysicalCandidate`, and compatibility views.
- Temporarily consumes `planner::seed_from_legacy` only in a `LegacyCandidateAdapter` used to prove losslessness.

- [ ] **Step 1: Write failing graph-invariant tests**

Construct a lowered fanout netlist and require one canonical instance per logical gate, ordered explicit assignments for every instance input and declared output, primary inputs as drivers, and no string identity. Add rejection tests for a missing assignment, duplicate assignment, wrong logical signal, duplicate reading different logical inputs, and a stateful gate.

The stateful test must match:

```rust
assert_eq!(
    InstanceGraph::one_to_one(&netlist, &library).unwrap_err(),
    SynthesisError::UnsupportedStatefulTopology { gate: GateIndex(0) },
);
```

- [ ] **Step 2: Define the candidate state keyed only by typed IDs**

Use:

```rust
pub struct ExpandedPhysicalCandidate {
    pub instances: InstanceGraph,
    pub placements: BTreeMap<PrimitiveId, PrimitivePlacement>,
    pub connections: BTreeMap<ConnectionId, ConnectionBinding>,
    pub routes: BTreeMap<RouteId, RealisedRouteTree>,
    pub junctions: BTreeMap<InstanceId, RealisedJunction>,
    pub observations: BTreeMap<ObservationId, VerifiedObservation>,
    pub pins: PortPlacements,
}
```

`VerifiedObservation` contains the Task 2 `ObservationSite` plus the verified emitted cell state; Task 3 does not implement simulator observation behavior. `PrimitivePlacement` carries the selected physical variant, anchor, and full emitted `BlockState` data. `ConnectionBinding` names the typed source, landing, route tree, and concrete routed sink. `RealisedRouteTree` owns a shared trunk once, ordered branches, full route/floor `BlockState`s, and one terminal record per `RoutedSinkId`; fanout branches must not duplicate their common trunk in candidate identity or block metrics. Canonical gate positions/facings are derived compatibility views only.

Delayed-component ownership is explicit and exclusive:

```rust
pub enum DelayedOwner {
    Primitive(PrimitiveId),
    Route(RouteId),
    InputBinding(PortId),
}
```

A topology repeater is never also charged to a route terminal, and a pinned input reader is never charged to its downstream route.

- [ ] **Step 3: Implement the migration adapter and byte-identical test**

Map one legacy seed into one-to-one instances, including bare/mixed merge ownership and terminal repeaters. Re-emit the adapted candidate and compare every world cell and compatibility coordinate with the original legacy result for NOT, and4, fanout, bare merge, and mixed merge. Exercise the two-node BUF directly through the new candidate constructor because the legacy lowered front door does not accept BUF as a final cell-level gate.

```powershell
cargo test --test compile_end_to_end a_typed_one_to_one_candidate_re_emits_the_exact_legacy_world -- --exact --nocapture
```

Expected before implementation: failure for the missing adapter. Expected after implementation: byte-identical worlds.

- [ ] **Step 4: Add canonical candidate identity distinct from world identity**

Fingerprint the complete ordered graph, implementation keys, topology fingerprints, assignments, endpoints, contributors, primitive placements/variants/facings, observations, pins, and routes. Add a test where two candidates emit identical block states but swap ownership of two same-shaped instances; candidate fingerprints must differ while emitted-world fingerprints match.

- [ ] **Step 5: Run representation regressions**

```powershell
cargo test --lib compile::fragment_synth::instance_graph::tests -- --nocapture
cargo test --lib compile::fragment_synth::candidate::tests -- --nocapture
cargo test --test compile_end_to_end -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 3**

```powershell
git add src/compile/fragment_synth src/compile/planner.rs tests/compile_end_to_end.rs
git commit -m "feat(synthesis): represent complete typed candidates"
```

---

### Task 4: Make structural certification independently re-instantiate topology

**Files:**
- Create: `src/compile/emission.rs`
- Create: `src/compile/verification.rs`
- Create: `src/compile/fragment_synth/realise.rs`
- Create: `src/compile/fragment_synth/verify.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/planner.rs`
- Modify: `src/compile/equivalence.rs`
- Modify: `src/compile/primitive_graph.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Test: `src/compile/verification.rs` (`tests` module)
- Test: `src/compile/fragment_synth/verify.rs` (`tests` module)

**Interfaces:**
- Produces durable `compile::emission` and `compile::verification` authorities plus `realise_and_verify_expanded(candidate, netlist, library) -> Result<CertifiedWorld, CertificationError>`.
- Reuses existing collision, coupling, strength, direction, connectivity, merge, and terminal-rule implementations through a common lossless `PhysicalCandidateView`; both legacy and fragment paths are adapters over that view.

- [ ] **Step 1: Write the required non-ignored corruption matrix**

Start from one certified candidate containing a two-node BUF and mixed merge. Clone and independently mutate it: remove an internal connection, redirect an internal connection to the wrong sink, remove and duplicate a primitive, change a primitive kind, reverse a primitive facing, add an extra primitive, mismatch implementation key/topology, reassign one external sink, omit a junction contributor, swap junction logical owner/contributor, delete a declared-output terminal, and falsify an observation point. Require a named `StructuralMismatch` carrying the affected stable ID before functional simulation starts.

- [ ] **Step 2: Run the corruption tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::verify::tests -- --nocapture
```

Expected: compilation fails because expanded certification does not exist.

- [ ] **Step 3: Extract durable emission and physical-verification authorities**

Move block emission into `compile::emission` and physical-rule orchestration into `compile::verification`, both outside deletable planner policy. Define a sealed, lossless `PhysicalCandidateView` consumed by these modules. Refactor the body of old `planner::realise_and_verify` and the `compile/mod.rs` net/source plus collision/connectivity/merge/terminal invariant inputs so `LegacyPlanAdapter<'_>` and `ExpandedCandidateAdapter<'_>` provide that same view. Do not convert typed endpoints to gate-name strings. The view retains shared route trees, internal routes, full route/floor/terminal `BlockState`, duplicate identities, junction contributors, observations, and pins. `planner::realise_and_verify` and `fragment_synth::realise` become thin adapters; neither owns an emitter or verifier rule.

- [ ] **Step 4: Re-instantiate and compare before emission**

For every instance, call `instantiate(library, logical_gate, instance.id, &instance.implementation)` and compare exact primitive, connection, output, and fingerprint sets to candidate state. Then prove each expected junction contributor reaches exactly one verified observation and no unlisted contributor joins it. Only after those checks emit the world and run the reused physical rules.

- [ ] **Step 5: Run corruption, coupling, strength, and terminal suites**

```powershell
cargo test --lib compile::fragment_synth::verify::tests -- --nocapture
cargo test --lib compile::verification::tests -- --nocapture
cargo test --test primitive_graph_equivalence -- --nocapture
cargo test --test terminal_handover -- --nocapture
cargo test --test or_merge -- --nocapture
cargo test --lib compile::coupling -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 4**

```powershell
git add src/compile/emission.rs src/compile/verification.rs src/compile/fragment_synth src/compile/mod.rs src/compile/planner.rs src/compile/equivalence.rs src/compile/primitive_graph.rs
git commit -m "feat(synthesis): certify expanded topology independently"
```

---

### Task 5: Extract the durable typed physical router and preserve repeater orientation

**Files:**
- Create: `src/compile/routing.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/fragment_synth/candidate.rs`
- Modify: `src/compile/fragment_synth/realise.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/routing.rs` (`tests` module)
- Test: `src/compile/fragment_synth/realise.rs` (`tests` module)
- Test: `tests/delay_model_reconciliation.rs`

**Interfaces:**
- Produces `PhysicalRouter`, typed `RouteRequest`, fixed `RouterLimits`, typed `RouteEndpoint`/`RouteSink`, `NonEmptyRouteSinks`, `PhysicalReservations`, `RealisedRouteTree`, and one route-step legality function in durable `compile::routing`, outside `planner.rs`.
- Both the old planner and fragment synthesiser call the same router through lossless adapters; neither owns a private copy of route physics.
- Guarantees a repeater may be entered only from its rear and left only through its front, based on `BlockState::facing`.

- [ ] **Step 1: Write failing typed-router and cap tests**

Call the new API with a `PhysicalEndpointId` source and ordered non-empty typed sinks. Require a fanout result to retain one shared trunk, ordered sink branches, every route-cell `BlockState`, every floor `BlockState`, and one full terminal record per `RoutedSinkId`. A request with `RouterLimits { max_node_expansions: 0, max_queue_entries: 0 }` must return a deterministic `RouterLimitExceeded` naming the route, source, next sink, limit kind, and work used; it may not fall back to an unbounded search.

Use these production shapes:

```rust
pub struct RouterLimits {
    pub max_node_expansions: u64,
    pub max_queue_entries: u64,
}

pub struct RouteRequest<'a> {
    pub id: RouteId,
    pub source: RouteEndpoint,
    pub sinks: &'a NonEmptyRouteSinks,
    pub reservations: &'a PhysicalReservations,
    pub limits: RouterLimits,
}

pub trait PhysicalRouter {
    fn route(&self, request: RouteRequest<'_>)
        -> Result<RealisedRouteTree, RouterFailure>;
}
```

`NonEmptyRouteSinks::new(Vec<RouteSink>)` returns `EmptyRouteSinks` for an empty vector and otherwise preserves caller order. `PhysicalReservations` is the durable read-only occupied/conductor/keep-out view extracted from the planner's current reservation tables. `RouteEndpoint` and `RouteSink` each carry `PhysicalEndpointId`, anchor, allowed entry/exit face, and terminal contract. `RealisedRouteTree` canonicalises cells by typed route ownership and stable branch order, not hash-map iteration.

- [ ] **Step 2: Write a failing axis-sensitive route test**

Build two otherwise identical candidate routes. One repeater faces along the predecessor-to-successor axis; the other is rotated 90 degrees. Require the first to certify and the second to return `WrongRepeaterAxis` naming the `ConnectionId` and anchor. Add a round-trip test asserting `kind`, `facing`, and `delay` survive route proposal, candidate storage, emission, and verification.

- [ ] **Step 3: Run the focused tests**

```powershell
cargo test --lib compile::routing::tests -- --nocapture
cargo test --lib compile::fragment_synth::realise::tests -- --nocapture
```

Expected: compilation fails because the durable typed router API does not exist.

- [ ] **Step 4: Extract routing physics and adapt both paths**

Move StrengthAware graph expansion, reservation checks, route-tree reconstruction, terminal selection, and limit accounting from `planner.rs` into `compile::routing`. `LegacyPlannerRouterAdapter` converts the legacy planner's current coordinates and string-labelled sinks into a fully typed request at its boundary and converts the returned tree back without dropping metadata. `FragmentRouterAdapter` passes fragment identities directly. Add parity tests proving the legacy adapter produces the same anchors, states, floors, terminals, and refusal category for existing and4/fanout fixtures, while the fragment adapter receives distinct typed sink identities.

- [ ] **Step 5: Centralise full-state conduction checks**

Implement:

```rust
fn route_step_is_legal(previous: Anchor, at: Anchor, next: Anchor, state: &BlockState) -> bool;
```

Use the same function in StrengthAware search, route reconstruction, expanded emission, and structural verification. Remove any route-state reconstruction that defaults repeater facing or delay.

- [ ] **Step 6: Run router, adapter, and delay reconciliation**

```powershell
cargo test --lib compile::routing::tests -- --nocapture
cargo test --lib compile::fragment_synth::realise::tests -- --nocapture
cargo test --test delay_model_reconciliation terminal_repeaters_are_every_repeater_on_the_path -- --exact --nocapture
cargo test --test compile_end_to_end -- --nocapture
```

Expected: all pass.

- [ ] **Step 7: Commit Task 5**

```powershell
git add src/compile/mod.rs src/compile/routing.rs src/compile/fragment_synth src/compile/planner.rs tests/delay_model_reconciliation.rs
git commit -m "fix(routing): retain repeater state through certification"
```

---

### Task 6: Derive the primitive-level realised timing graph

**Files:**
- Create: `src/compile/fragment_synth/timing_graph.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/fragment_synth/identity.rs`
- Modify: `src/compile/fragment_synth/candidate.rs`
- Modify: `src/timing/mod.rs`
- Modify: `src/redstone/simulator/mod.rs`
- Modify: `src/redstone/simulator/observer.rs`
- Test: `src/compile/fragment_synth/timing_graph.rs` (`tests` module)
- Test: `tests/delay_model_reconciliation.rs`

**Interfaces:**
- Produces `TimingNodeId`, `TimingArcId`, `TimingArcKind`, `ExactDelay`, `RealisedTimingGraph`, `StaticTiming`, observer behavior over Task 2's `ObservationId`/`ObservationSite`, and `TransitionWitness`.
- Consumes only a certified expanded candidate and its immutable instance graph.

- [ ] **Step 1: Write graph-shape and weighted-path tests**

Require these exact paths:

- BUF: external landing -> first torch -> internal route -> second landing -> second torch -> instance output;
- mixed merge: isolated branch landing -> repeater -> junction, while the bare branch landing -> junction;
- pinned input: one binding repeater before downstream route counts;
- pinned output: route-owned delivery repeater reaches declared output with no extra generic gate charge;
- unpinned output: zero-delay output binding.

Add the measured full-adder tie regression: if `g19` and `g20` both arrive at 34 but only the `g19` edge adds three repeaters, predecessor selection must choose `g19` by `head + edge_delay`.

- [ ] **Step 2: Run focused timing tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::timing_graph::tests -- --nocapture
```

Expected: compilation fails because the graph does not exist.

- [ ] **Step 3: Implement stable nodes, arcs, and max-plus analysis**

Build one route arc per concrete sink, one primitive arc per signal-carrying landing, zero-delay topology/junction/output boundaries, and explicit pinned bindings. Define `ExactDelay(u64)` in simulator game ticks and use it for arc delay, critical delay, quality, and serialised metrics. Reject cycles, unresolved owners, duplicate arc IDs, and any delayed primitive without an observation. Compute `head`, `tail`, `critical_delay`, and edge slack with stable ID tie-breaking.

- [ ] **Step 4: Make observation identity typed end to end**

Add typed observer registration through `Simulator::attach_typed_observer`, raw observation events, and a typed transition result so timelines are keyed by the already-defined `ObservationId`, with human labels as metadata. Store Task 2 `ObservationSite`s as `Position -> Vec<ObservationSite>` so an ordinary instance output may alias its selected primitive at the same coordinate without either identity disappearing. Define a total one-to-one conversion between observable `TimingNodeId` variants and `ObservationId`; landings remain intentionally non-observable. Keep existing `Simulator::attach_observer((Position, String))` and string-labelled timing APIs as compatibility wrappers rather than collapsing typed identities. Extend every `CompiledCircuit` constructor with a `CircuitObservations` value containing primitive, concrete instance, and junction maps keyed by typed IDs; retain the current gate-output map as the canonical compatibility projection. Retain every transition index tied for worst settle time, while keeping the lowest index as the compatibility scalar. Two duplicates with the same display label, and two typed identities at the same position, must retain independent event sequences.

- [ ] **Step 5: Reconcile metadata, graph, and simulator**

Add non-ignored tests for and4, full_adder, two-node BUF, mixed merge, pinned input, and pinned output. Assert every route-owned repeater is charged once, every topology repeater is charged once, and the old pinned-output double count is absent.

Add witness regressions requiring every tied-worst transition to remain available; when a declared output does not change, choose the latest changing internal primitive or primary input, and prefer an active witness over `StaticFallback`.

```powershell
cargo test --lib compile::fragment_synth::timing_graph::tests -- --nocapture
cargo test --test delay_model_reconciliation -- --nocapture
cargo test --test timing -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 6**

```powershell
git add src/compile/fragment_synth src/compile/mod.rs src/timing/mod.rs src/redstone/simulator/mod.rs src/redstone/simulator/observer.rs tests/delay_model_reconciliation.rs
git commit -m "feat(timing): model realised primitive paths"
```

---

### Task 7: Add immutable manifests, compositional equivalence, fixed caps, and complete certification

**Files:**
- Modify: `src/compile/fragment_synth/manifest.rs`
- Create: `src/compile/fragment_synth/config.rs`
- Create: `src/compile/fragment_synth/certification.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/fragment_synth/instance_graph.rs`
- Modify: `src/compile/fragment_synth/topology.rs`
- Modify: `src/compile/equivalence.rs`
- Test: `src/compile/fragment_synth/manifest.rs`, `config.rs`, and `certification.rs` (`tests` modules)
- Test: `src/compile/equivalence.rs` (`tests` module)

**Interfaces:**
- Produces `TransitionManifest`, fixed `SearchConfig`, `CertificationConfig`, `EquivalenceCertificate`, `CertifiedCandidate`, `CandidateMetrics`, and fresh-simulator transition sweeps.
- Promotes a candidate only after structural, functional, and complete manifest certification.

- [ ] **Step 1: Write exact manifest tests**

For 0-4 inputs, assert every ordered pair of distinct vectors and exactly 240 transitions for four inputs. Above four inputs, require all-zero, all-one, one-hot, one-cold, and every single-bit toggle among those vectors. Assert deterministic order and hash independence from candidate data.

- [ ] **Step 2: Write fresh-simulator and functional-gate tests**

Create a circuit whose shared simulator historically retains an event but whose fresh simulator settles correctly. Require each manifest entry to clone the certified world, apply source as a batch, settle, record `start_tick`, apply destination as a batch, and measure quiescence. Require divergence to return `TransitionDidNotSettle`, not a capped score.

For up to eight inputs, exhaustively compare outputs. Add a nine-primary-input combinational tree fixture that cannot use the exhaustive path. Its correct typed candidate must receive an `EquivalenceCertificate`; a clone with one sink deliberately assigned to a different, non-equivalent primary input must return `EquivalenceProofFailure` and remain non-promotable. Add separate failures for a duplicate whose ordered logical inputs differ from its canonical instance and for an `ImplementationKey` whose independently instantiated topology semantics do not implement the selected logical gate. Assert the proof path does not call the truth-table enumerator.

- [ ] **Step 3: Run tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::manifest::tests -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: compilation fails because certification types do not exist.

- [ ] **Step 4: Implement the production compositional equivalence proof**

Implement `prove_combinational_equivalence(lowered, candidate, library, limits) -> Result<EquivalenceCertificate, EquivalenceError>` in production `compile::equivalence`. This is an all-input proof, not a transition sample:

1. Reject stateful gates, cycles, unresolved drivers, duplicate/missing assignments, and work beyond `max_equivalence_proof_steps` before a certificate can exist.
2. Traverse the lowered combinational DAG and the one-to-one-or-duplicated `InstanceGraph` in stable topological/typed-ID order. The induction hypothesis maps each primary input and already-proved logical signal to its symbolic Boolean meaning for every possible input vector.
3. For every physical instance, independently call `instantiate` for its selected `ImplementationKey`; compose the declared primitive semantics and every internal connection in `ValidatedTopology`, then prove that output or junction semantics equal the logical gate's NOR/BUF/Merge function under the induction hypothesis. A route contributes identity only after structural certification proved its connectivity.
4. For every duplicate, prove its ordered input meanings equal its canonical instance's ordered logical inputs before admitting its output as another implementation of the same signal. Prove each `SinkAssignment` expects the same logical meaning its selected physical driver carries.
5. Prove each declared output binding equals the lowered netlist output. Only then return a certificate containing lowered-netlist hash, candidate fingerprint, library revision, ordered input axioms, one proof step per instance/duplicate/assignment/output, work used, and a canonical certificate fingerprint.

Because each primitive truth relation is checked symbolically and each DAG step preserves equality, induction proves every input vector without enumerating `2^n` rows. Proof exhaustion, a wrong assignment, duplicate mismatch, unknown primitive semantics, selected-implementation mismatch, or output mismatch returns a named error and cannot produce `CertifiedCandidate` or `best_certified`.

- [ ] **Step 5: Define every deterministic internal cap in `SearchConfig`**

Use fixed integer work units and checked defaults; none may be derived from `SynthesisBudget` or the wall clock:

```rust
pub struct SearchConfig {
    pub router_limits: RouterLimits,                 // 262_144 expansions, 262_144 queue entries
    pub max_seed_shell_radius: u32,                  // 64
    pub max_fragment_shell_radius: u32,              // 32
    pub max_seed_backtracks: u64,                    // 1_000_000
    pub max_fragment_backtracks_per_proposal: u64,   // 100_000
    pub max_equivalence_proof_steps: u64,            // 1_000_000
    pub max_certification_transitions: u64,           // 65_536
    pub max_simulator_events_per_transition: u64,    // 1_000_000
    pub max_game_ticks_per_transition: u64,          // 2_048
    pub fragment_instance_schedule: Vec<u16>,        // [1, 2, 4, 8]
    pub max_boundary_nets: u16,                      // 32
    pub max_fragment_manhattan_radius: u32,          // 48
}
```

Expose these exact `checked_defaults()` values in one literal constructor, not environment variables. Each work counter increments at a specified stable operation: router queue pop/push, shell candidate visited, backtrack edge traversed, proof step appended, transition started, simulator event processed, or game tick advanced. Exhaustion is a structured terminal refusal with used/limit values. `CertificationConfig` owns immutable policy such as the exhaustive-input threshold and transition-manifest kind; it receives the caps above rather than duplicating or hiding them. Canonically serialise the full configs for `SynthesisCaseFingerprint` in Task 9. Tests mutate each field independently and require a different config fingerprint.

- [ ] **Step 6: Implement certification and metrics**

Define the lexicographic quality key:

```rust
pub struct QualityKey {
    pub observed_settle: u64,
    pub non_air_blocks: u64,
    pub occupied_volume: u64,
    pub static_routed_delay: ExactDelay,
}
```

Record manifest hash, count, cap, all worst transition indexes, equivalence-certificate fingerprint (for the above-eight-input path), realised timing graph fingerprint, canonical candidate fingerprint, and emitted-world fingerprint. Candidate fingerprints only break complete quality ties and never count as improvement. Read a caller-owned pin as high when its observed signal strength is `> 0`; add tests at strengths 1 and 15 that both mean high and strength 0 means low.

- [ ] **Step 7: Run proof, config, certification, and truth-table regressions**

```powershell
cargo test --lib compile::equivalence::tests -- --nocapture
cargo test --lib compile::fragment_synth::config::tests -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
cargo test --test compile_end_to_end -- --nocapture
cargo test --test reference_circuits -- --nocapture
cargo test --test seven_segment -- --nocapture
```

Expected: all pass.

- [ ] **Step 8: Commit Task 7**

```powershell
git add src/compile/fragment_synth src/compile/equivalence.rs
git commit -m "feat(synthesis): certify fixed transition workloads"
```

---

### Task 8: Implement the independent topology-aware sparse seed

**Files:**
- Create: `src/compile/fragment_synth/seed.rs`
- Modify: `src/compile/fragment_synth/legacy_adapter.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/fragment_synth/seed.rs` (`tests` module)
- Create: `tests/fragment_synth_architecture.rs`
- Test: `tests/build_circuit_pins.rs`

**Interfaces:**
- Produces `SparseSeedBuilder::build(SynthesisInput) -> Result<CertifiedCandidate, SeedError>`.
- Consumes Task 7's fixed `SearchConfig`, durable `PhysicalRouter`, and expanded realisation/certification, but has no old-generator service or old placement-policy type in its constructor or module API.

- [ ] **Step 1: Enforce a compile-time API boundary and behavioral no-legacy test**

Define the seed constructor over a sealed `SeedServices<'a>` that contains only `&Library`, `&dyn PhysicalRouter`, `&dyn ExpandedCandidateCertifier`, and `&SearchConfig`. Keep `LegacyCandidateAdapter` and a private `LegacyOracle` trait in `fragment_synth::legacy_adapter`, compiled only for baseline/differential migration use and not re-exported from `fragment_synth`; `seed` receives neither type. `tests/fragment_synth_architecture.rs` is a normal compile-time public-API test that constructs `SparseSeedBuilder` solely from the durable services. Do not inspect Rust source text.

Add an injected `CountingLegacyOracle` in `legacy_adapter.rs`'s internal test module, where private migration APIs are visible. Run `compile_fragment_synth` at budget zero through spy router/certifier services and require `legacy_oracle.calls() == 0`; then invoke the explicit differential adapter once and require exactly one call. This behavioral test fails if the new front door delegates seed construction to the old generator, while the sealed public API makes accidental injection into `SparseSeedBuilder` a compile-time type error.

- [ ] **Step 2: Write deterministic seed tests**

For NOT, and4, fanout, two-node BUF, bare merge, mixed merge, fully isolated merge, and pinned and4, build twice and require byte-identical candidate/world fingerprints, full certification, exact pin coordinates, and a complete `InstanceGraph`. Add a stateful rejection test before any placement and a named bounded exhaustion test carrying instance, primitive, and radius.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
```

Expected: compilation fails because the independent builder does not exist.

- [ ] **Step 4: Implement deterministic ordering and placement**

Process canonical instances in stable topological order, primitives in `TopologyNodeId` order, facings in fixed NESW order, physical variants by library order, and anchors in expanding Manhattan shells around deterministic barycentres of already placed sources/sinks. Place pinned handover primitives first and never move their caller cells. Stop at `max_seed_shell_radius` and `max_seed_backtracks` from `SearchConfig`; do not read the optimisation budget.

Every trial checks physical-variant footprint and keep-out constraints locally. If the completed candidate fails authoritative certification, backtrack deterministically to the next placement choice; report the first stable refusal identity when the configured seed radius is exhausted.

- [ ] **Step 5: Implement topology-aware routing**

Route external and internal `ConnectionId`s in stable critical-estimate/fanout/ID order using Task 5's typed `PhysicalRouter` and its fixed `router_limits`. Internal edges are ordinary routing obligations. Materialise junction contributors explicitly. After all routes exist, call `realise_and_verify_expanded` and full certification; only that result is the seed.

- [ ] **Step 6: Prove zero-budget behaviour and run pin regressions**

Expose a temporary `compile_sparse_seed` test helper and require it to return the certified seed without starting optimisation.

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo test --test fragment_synth_architecture -- --nocapture
cargo test --test build_circuit_pins -- --nocapture
cargo test --test terminal_handover -- --nocapture
```

Expected: all pass.

- [ ] **Step 7: Commit Task 8**

```powershell
git add src/compile/fragment_synth src/compile/planner.rs tests/fragment_synth_architecture.rs tests/build_circuit_pins.rs
git commit -m "feat(synthesis): build independent sparse seeds"
```

---

### Task 9: Add deterministic budget accounting with a no-op proposal stream

**Files:**
- Create: `src/compile/fragment_synth/search.rs`
- Create: `src/compile/fragment_synth/api.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/mod.rs`
- Test: `src/compile/fragment_synth/search.rs` (`tests` module)

**Interfaces:**
- Produces `SynthesisInput`, `SynthesisBudget`, `SynthesisCaseFingerprint`, `SynthesisResult`, `StopReason`, `ProposalTrace`, and explicit `compile_fragment_synth`.
- Consumes the complete Task 7 `SearchConfig` and `CertificationConfig`; every cap is part of the case fingerprint and remains constant across evaluation/time budgets.
- Initially enumerates deterministic no-op/refused proposals so budget semantics land before search policy.

- [ ] **Step 1: Write budget-prefix and best-retention tests**

Run fresh syntheses at evaluation budgets 0, 1, 2, 4, and 8 under one explicit `SearchConfig`. Require budget 0 to return the certified seed; traces at smaller budgets to be byte-identical prefixes of larger traces; completed evaluations never exceed budget; and final quality never worsen. Inject refused, router-cap-exhausted, backtrack-cap-exhausted, proof-cap-exhausted, verification-failed, and certification-cap-exhausted proposals and require each to record one deterministic terminal outcome without erasing `best_certified`. Mutate each internal-cap field separately and require a different `SynthesisCaseFingerprint` before comparing traces.

- [ ] **Step 2: Write time-budget equivalence tests around a fake clock**

Use an injected monotonic clock in unit tests with the identical `SearchConfig`, seed, and proposal enumerator as evaluation mode. If time mode starts and finishes `k` proposals, require its complete trace, terminal outcomes, cap work counters, accepted candidate, and result fingerprints to equal `Evaluations(k)`. Advance the fake deadline during a transaction and require that proposal to finish before `StopReason::TimeBudget` is returned. This test is the required time/evaluation prefix-equivalence proof; elapsed time is the only excluded field.

- [ ] **Step 3: Run tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::search::tests -- --nocapture
```

Expected: compilation fails because the state machine does not exist.

- [ ] **Step 4: Implement the loop-boundary budget state machine**

Compute `SynthesisCaseFingerprint` from lowered netlist and port order, pins, library revision, the complete canonical `SearchConfig` including every Task 7 cap, `CertificationConfig`, simulator/verifier revisions, and manifest hash. Read the requested budget only at the loop boundary. Record proposal index, parent fingerprint, fragment ID, choice fingerprint, terminal outcome, cap work used, certified quality, and accepted flag.

- [ ] **Step 5: Expose the advanced API without changing shipping paths**

```rust
pub fn compile_fragment_synth(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
) -> Result<SynthesisResult, SynthesisError>;

pub struct SynthesisInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
}
```

`SynthesisResult` owns `pub compiled: CompiledCircuit` together with metrics, trace, budget use, fingerprints, and stop reason, so compatibility front doors can return `.compiled` without rerunning synthesis. Keep `compile`, `compile_grown`, CLI, baker, and viewer unchanged. Add `PlannerKind::FragmentSynth` only to results from this explicit API.

- [ ] **Step 6: Run deterministic API tests**

```powershell
cargo test --lib compile::fragment_synth::search::tests -- --nocapture
cargo test --test compile_end_to_end -- --nocapture
```

Expected: all pass and existing production planner-kind expectations remain unchanged.

- [ ] **Step 7: Commit Task 9**

```powershell
git add src/compile/mod.rs src/compile/fragment_synth
git commit -m "feat(synthesis): add deterministic budgeted search API"
```

---

### Task 10: Implement timing-guided single-instance fragment transactions

**Files:**
- Create: `src/compile/fragment_synth/fragment.rs`
- Modify: `src/compile/fragment_synth/search.rs`
- Modify: `src/compile/fragment_synth/timing_graph.rs`
- Test: `src/compile/fragment_synth/fragment.rs` (`tests` module)

**Interfaces:**
- Produces stable hotspot ranking, route-tree closure, `FragmentId`, `FragmentChoice`, and atomic single-instance replacement.
- Uses static critical slack plus complete worst-transition witnesses; simulator score remains authoritative.

- [ ] **Step 1: Write hotspot and closure tests**

Create a fanout route tree where the critical sink shares a trunk with a non-critical sink. Starting from the critical timing arc must include the full shared route-tree closure and boundary drivers/sinks. Add equal-score hotspots and require stable `TimingArcId` order. Add an unsensitised static path and require dynamic witnesses to rank an active tied-worst path first while retaining static fallback when no event exists.

- [ ] **Step 2: Write transaction atomicity tests**

For a one-instance fragment, force failures at materialisation, routing, structural verification, functional certification, and quality comparison. After each failure assert parent candidate, timing graph, best fingerprint, and trace parent fingerprint are unchanged. A successful replacement must rebuild all affected topology, routes, observations, timing graph, and metrics.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::fragment::tests -- --nocapture
```

Expected: compilation fails because fragments do not exist.

- [ ] **Step 4: Implement stable proposal enumeration**

Rank zero-slack/low-slack arcs, delayed primitive events, glitches, and worst targets using complete tied-worst witnesses. Expand to bounded instance plus route-tree closure. Enumerate implementation choices, facings, variants, anchor shells no larger than `max_fragment_shell_radius`, no more than `max_fragment_backtracks_per_proposal`, and reroute choices under the same fixed `router_limits`, all in stable typed-ID order. The fragment and choice canonical encodings form their fingerprints; any cap refusal is one terminal proposal outcome.

- [ ] **Step 5: Implement private materialise-route-certify-commit**

Clone only into transaction state, re-instantiate changed instances, delete stale physical/timing/observation records, reroute every affected internal/external connection, then run full structural and functional certification. Commit only if `QualityKey` is strictly smaller; equal quality keeps the canonical fingerprint representative without incrementing accepted improvements.

- [ ] **Step 6: Run fragment and budget regressions**

```powershell
cargo test --lib compile::fragment_synth::fragment::tests -- --nocapture
cargo test --lib compile::fragment_synth::search::tests -- --nocapture
```

Expected: all pass.

- [ ] **Step 7: Commit Task 10**

```powershell
git add src/compile/fragment_synth
git commit -m "feat(synthesis): optimise certified single-instance fragments"
```

---

### Task 11: Add multi-instance fragments and combinational duplication

**Files:**
- Modify: `src/compile/fragment_synth/instance_graph.rs`
- Modify: `src/compile/fragment_synth/fragment.rs`
- Modify: `src/compile/fragment_synth/search.rs`
- Modify: `src/compile/fragment_synth/verify.rs`
- Test: `src/compile/fragment_synth/fragment.rs` (`tests` module)

**Interfaces:**
- Produces deterministic duplicate IDs, sink partition proposals, and multi-instance atomic rerouting.
- Restricts duplication to combinational instances with one concrete output primitive; merge/junction duplication is rejected by name.

- [ ] **Step 1: Write duplicate identity and legality tests**

Duplicate a high-fanout NOR. Require `InstanceRole::Duplicate { ordinal: 1 }`, newly derived primitive/connection IDs, identical logical inputs, independent observations, and a deterministic sink partition. Reject changed logical inputs, duplicate IDs, missing canonical instance, stateful duplication, and merge duplication.

- [ ] **Step 2: Write beneficial and non-beneficial fanout tests**

Use one fixture where separating two far consumers removes enough route repeaters to improve settle ticks or block count, and one compact fixture where copied cell cost loses. Require only the first to commit and both to remain fully certified.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::fragment::tests -- --nocapture
```

Expected: compilation fails because duplicate proposals do not exist.

- [ ] **Step 4: Implement deterministic multi-instance transactions**

Assign duplicate ordinals monotonically within a candidate, partition sinks in stable `PhysicalSink` order, rebuild duplicate topology from the same `ImplementationKey`, reroute every duplicate input and affected output tree, and independently verify each duplicate's cell, ports, routes, and observations.

- [ ] **Step 5: Re-run prefix and certification suites**

```powershell
cargo test --lib compile::fragment_synth::fragment::tests -- --nocapture
cargo test --lib compile::fragment_synth::search::tests -- --nocapture
cargo test --test primitive_graph_equivalence -- --nocapture
```

Expected: all pass; budget traces remain prefix-compatible after duplication choices join the proposal order.

- [ ] **Step 6: Commit Task 11**

```powershell
git add src/compile/fragment_synth
git commit -m "feat(synthesis): duplicate critical combinational instances"
```

---

### Task 12: Build and run the replacement-gate harness

**Files:**
- Modify: `src/compile/fragment_synth/benchmark.rs`
- Modify: `src/bin/fragment_baseline.rs`
- Create: `src/bin/fragment_acceptance.rs`
- Create: `tests/fragment_synth_acceptance.rs`
- Create: `tests/fixtures/fragment_synth_shipping.json`
- Create only on gate pass: `src/compile/fragment_synth/shipping_config.rs`
- Modify: `tests/fixtures/fragment_synth_baseline.json` only if its schema gains fields, never its recorded baseline values

**Interfaces:**
- Produces a machine-readable acceptance report plus a generated, checked production `shipping_config.rs` containing the exact shipping evaluation budget/config only when every spec condition passes.
- Compares legacy baseline and new candidates with the exact same evaluator and fixtures.

- [ ] **Step 1: Write failing correctness-corpus assertions**

Require independent new seed, route, realisation, verification, truth/equivalence proof, complete manifest sweep, and pin checks for and4, verilog:and4, full_adder, segment_a, handwritten seven_segment, and pinned verilog:seven_segment with checked-in glyph pins. No acceptance test is ignored and no `Err` is printed without failing.

- [ ] **Step 2: Write exact quality-gate arithmetic**

For every baseline-certified case require no settle-tick or non-air regression. For pinned verilog:seven_segment require widened arithmetic:

```rust
assert!(10_u128 * new_ticks as u128 <= 9_u128 * baseline_ticks as u128);
assert!(new_blocks < baseline_blocks);
```

Record occupied volume without making it an independent failure. Cases without numeric baseline must still certify and report `new_coverage: true`.

- [ ] **Step 3: Add cross-process budget monotonicity**

Run 0, 1, 2, 4, 8, and candidate shipping budgets in shuffled order, each in at least three fresh processes. Use the checked base shuffle seed `0x5245_4441_2026_0831`; derive each repetition's seed by adding its zero-based repetition index with wrapping `u64` arithmetic, then apply deterministic Fisher-Yates over the sorted budget list. Record the base seed, each derived seed, and each actual budget order in the JSON acceptance report. Compare trace, cap work counters, metrics, candidate fingerprint, and emitted-world fingerprint byte-for-byte while excluding elapsed time. Require used evaluations <= budget and non-worsening quality.

Add harness tests that a different recorded order fails repeatability checking and that a failed gate leaves a fresh temporary `shipping_config.rs` path absent. On pass, generate this production source atomically from the measured report with literal `SHIPPING_EVALUATIONS`, full `SHIPPING_SEARCH_CONFIG`, full `SHIPPING_CERTIFICATION_CONFIG`, synthesis-case fingerprint, base shuffle seed, and recorded orders. Byte-compare a second generation before accepting it; no value may be selected dynamically at runtime.

- [ ] **Step 4: Run the harness from a clean checkout**

```powershell
git status --short
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output tests/fixtures/fragment_synth_shipping.json --shipping-source src/compile/fragment_synth/shipping_config.rs --shuffle-seed 0x5245444120260831
```

Expected: `git status --short` is empty before the run; the report explicitly says either `replacement_gate_passed: true` with the smallest passing checked evaluation budget and a byte-stable generated production config source, or `false` with named failing cases/conditions and no `shipping_config.rs`.

- [ ] **Step 5: Commit only measured acceptance data**

If the gate passes, commit the harness, report, and generated production config source:

```powershell
git add src/compile/fragment_synth/benchmark.rs src/compile/fragment_synth/shipping_config.rs src/bin/fragment_baseline.rs src/bin/fragment_acceptance.rs tests/fragment_synth_acceptance.rs tests/fixtures/fragment_synth_shipping.json
git add tests/fixtures/fragment_synth_baseline.json
git commit -m "test(synthesis): enforce the replacement gate"
```

If it fails, first assert `Test-Path src/compile/fragment_synth/shipping_config.rs` is false, then commit only the harness and failure report; do not create a shipping budget, production config source, or front-door change:

```powershell
git add src/compile/fragment_synth/benchmark.rs src/bin/fragment_baseline.rs src/bin/fragment_acceptance.rs tests/fragment_synth_acceptance.rs tests/fixtures/fragment_synth_shipping.json
git add tests/fixtures/fragment_synth_baseline.json
git commit -m "test(synthesis): record replacement gate failure"
```

---

### Task 13: Conditionally switch front doors, regenerate artifacts, and delete legacy generation

**Precondition:** `tests/fixtures/fragment_synth_shipping.json` exists, says `replacement_gate_passed: true`, records the smallest passing fixed evaluation budget/config and shuffle evidence, `src/compile/fragment_synth/shipping_config.rs` was generated byte-for-byte from that passing report, and Task 12 passes from a clean checkout. If any condition is false, stop this plan after Task 12 with the explicit new API intact.

**Files:**
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Read unchanged: `src/compile/fragment_synth/shipping_config.rs` generated by Task 12
- Modify: `src/bin/build_circuit.rs`
- Modify: `src/bin/mc_dump.rs`
- Modify: `viewer/src/lib.rs`
- Modify: `viewer/tests/*.rs` where planner-kind expectations change
- Modify: `tests/fragment_synth_architecture.rs`
- Modify: `tests/fragment_synth_acceptance.rs`
- Modify: checked-in baked `.litematic` and pinout sidecars produced by `build_circuit`
- Delete: legacy generator/fallback code only in a separate commit after switchover verification
- Modify: tests and reports that intentionally target deleted legacy APIs

**Interfaces:**
- `compile(&Netlist)` calls fragment synthesis with no source provenance, no pins, and the checked production shipping config.
- `compile_grown(&Netlist, &PortPlacements)` becomes a compatibility wrapper with no source provenance and exactly the supplied pins.
- `compile_fragment_synth` remains the advanced result/trace API.
- `PlannerKind` exposes only shipping fragment synthesis as a public generation policy after deletion.

- [ ] **Step 1: Assert the precondition in code and tests**

Declare `fragment_synth::shipping_config` and compile the literal generated production source. Production code must not open, parse, `include_str!`, or otherwise depend on `tests/fixtures/fragment_synth_shipping.json` or any path under `tests/fixtures`. The integration test may parse the JSON report and assert every budget/config/fingerprint/shuffle field equals the compiled constants. Add an architecture assertion that production source contains no `tests/fixtures` dependency. Do not choose a budget dynamically.

- [ ] **Step 2: Switch library, CLI, baker, and viewer callers**

Use distinct calls at each boundary; do not copy one all-`Some` example everywhere:

```rust
// `compile`: its argument is already lowered and has no pins.
compile_fragment_synth(
    SynthesisInput { lowered: netlist, source_provenance: None, pins: None },
    SynthesisBudget::Evaluations(SHIPPING_EVALUATIONS),
).map(|result| result.compiled)

// `compile_grown`: its argument is already lowered and the caller supplied pins.
compile_fragment_synth(
    SynthesisInput { lowered: netlist, source_provenance: None, pins: Some(placements) },
    SynthesisBudget::Evaluations(SHIPPING_EVALUATIONS),
).map(|result| result.compiled)

// CLI/viewer paths that begin with source gates lower with provenance first.
let (lowered, provenance) = lower_with_provenance(source)?;
let synthesis = compile_fragment_synth(
    SynthesisInput {
        lowered: &lowered,
        source_provenance: Some(&provenance),
        pins: requested_pins.as_ref(),
    },
    SynthesisBudget::Evaluations(SHIPPING_EVALUATIONS),
)?;
```

`build_circuit`, `mc_dump`, and viewer must use a provenance-preserving lowering variant whenever they start from source gates. Preserve already-lowered input semantics, supplied pins, structured errors, and caller-owned pin cells. Update viewer annotations so one logical gate may list multiple instance outputs while the canonical instance remains the compatibility position.

- [ ] **Step 3: Regenerate artifacts and run complete pre-deletion verification**

```powershell
cargo test --workspace --all-targets
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo test --manifest-path viewer/Cargo.toml
cargo build --manifest-path viewer/Cargo.toml --target wasm32-unknown-unknown
```

Run the repository's checked-in artifact generation command for the baked circuits and inspect the complete artifact/sidecar diff before committing.

- [ ] **Step 4: Commit the front-door switch separately**

```powershell
git add src/compile/mod.rs src/compile/fragment_synth/mod.rs src/bin viewer tests
git add -u
git commit -m "feat(compile): ship timing-directed fragment synthesis"
```

- [ ] **Step 5: Prove durable dependencies are independent, then delete only legacy policy**

Before deleting anything, extend `tests/fragment_synth_architecture.rs` so fragment synthesis, `compile::routing`, `compile::emission`, and `compile::verification` compile and run without importing a legacy planner-policy module. Confirm the fragment path reaches each durable router/emitter/verifier call spy once on a certified seed. Move any still-shared helper out of `planner.rs` or legacy `compile/mod.rs` sections into those durable modules, with parity tests, before proceeding.

Then remove `compile_legacy`, old planner-selection fallback, spring/growth/negotiated placement and optimisation policy, `LegacyCandidateAdapter`, and tests/reports that exist only to select those policies. Retain `geometry::Anchor`, typed routing physics, emission, verifier rules, simulator, topology library, physical variants, and diagnostic APIs used by the new synthesiser. The deletion diff must contain policy/adapters/fallback only; do not combine this deletion with the front-door switch commit.

- [ ] **Step 6: Re-run the complete suite after deletion**

```powershell
cargo test --workspace --all-targets
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo test --manifest-path viewer/Cargo.toml
cargo build --manifest-path viewer/Cargo.toml --target wasm32-unknown-unknown
```

Expected: all pass and the acceptance report remains byte-identical apart from explicitly excluded elapsed time.

- [ ] **Step 7: Commit deletion**

```powershell
git add -u
git add src tests viewer
git commit -m "refactor(compile): remove superseded generators"
```

---

## Final Review Gates

- [ ] Map every normative item in spec sections 3-15 to at least one task/test above; record the mapping in the implementation PR description.
- [ ] Search new production code for forbidden old-generator calls and confirm they occur only in baseline/differential migration tests before Task 13.
- [ ] Search all new files for unfinished-work markers and resolve every hit before execution is called complete.
- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets`.
- [ ] Confirm every acceptance test is non-ignored, every candidate promoted to best owns a certified world, and every rejected proposal leaves the parent fingerprint unchanged.
- [ ] Confirm clean-worktree Task 12 evidence exists before performing any Task 13 deletion.
