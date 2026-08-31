# Timing-Directed Fragment Synthesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a deterministic, topology-aware fragment synthesiser that always returns a certified sparse seed, spends larger evaluation or time budgets on a prefix-compatible optimisation stream, and ranks candidates first by observed redstone latency and then by emitted size.

**Architecture:** Add a new `compile::fragment_synth` subsystem beside the current generators. It owns typed physical identities, pure cell-topology expansion, a complete candidate model, sparse seed construction, primitive-level timing, certification, and fragment search. Existing cell-library data, routing physics, emitter, verifier rules, simulator, and pinned-IO contract remain authoritative but are reached through lossless typed adapters. The shipping `compile` path stays unchanged until the recorded replacement gate passes.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, SHA-256 canonical fingerprints, REDA cell topology and physical variants, existing redstone simulator and verifier rules, Cargo unit/integration tests.

**Spec:** `docs/superpowers/specs/2026-08-31-timing-directed-fragment-synthesis.md`

## Global Constraints

- This milestone accepts lowered, combinational netlists only. Return `UnsupportedStatefulTopology` before `InstanceGraph` construction and preserve the existing named errors for combinational cycles and undriven signals.
- The new generator must not call `compile_legacy`, `compile_planned`, `compile_grown`, `seed_from_legacy`, `plan_from_netlist`, spring relaxation, or the old optimiser after Task 7's independent seed exists. The legacy adapter in Tasks 3-4 is migration and differential-test code only.
- Do not migrate `PrimitiveId` into `relax/build.rs` or `relax/snap.rs`. Those files remain part of the legacy oracle until deletion; the new `SparseSeedBuilder` owns typed primitive placement directly. The one-to-one migration adapter observes a completed legacy seed and never makes legacy relaxation part of new candidate construction.
- Every externally visible candidate is complete, independently re-instantiated from the immutable library, physically verified, functionally certified, and transition-measured. Rejected transaction state never mutates the accepted parent.
- Logical ownership, electrical endpoint identity, and physical coordinates are separate fields. No map key may infer one from another.
- Every topology primitive and internal edge is materialised, routed, timed, observed when delayed, and structurally verified. `Template::output = None` creates a junction, never a fictional gate body.
- `ImplementationKey::Merge { isolation_mask }` is the one parameterised implementation of bare, mixed, and fully isolated merges. The mask and expanded-topology fingerprint participate in candidate identity.
- Pinned cells remain caller-owned air in the emitted world. Input travel is `at + toward`; output travel is `at - toward`; only that handover neighbour may carry the pin signal.
- Static timing counts simulator game ticks from actual primitive and route-owned repeaters. Dust geometry has zero timing delay. Pinned input and output repeaters are each charged exactly once.
- Evaluation budgets are deterministic and prefix-compatible. Time budgets run the same proposal stream and inspect the deadline only before starting a whole transaction.
- `World::size()` is storage allocation, not physical size. Shared metrics count non-air blocks and the occupied bounding box.
- Do not change production front doors or remove a legacy path until Task 13's clean-checkout replacement gate passes. A failed gate leaves the experiment explicit and isolated.

---

### Task 0: Add canonical fingerprints and emitted-world metrics

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/compile/mod.rs`
- Create: `src/compile/metrics.rs`
- Test: `src/compile/metrics.rs` (`tests` module)

**Interfaces:**
- Produces `Fingerprint`, `Ratio`, `PhysicalMetrics`, `canonical_fingerprint`, and `physical_metrics`.
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

Also require an all-air world to report zero blocks, volume zero, and both bounds as `None`; this avoids inventing a coordinate for an empty circuit.

- [ ] **Step 2: Run the focused test and verify RED**

```powershell
cargo test --lib compile::metrics::tests -- --nocapture
```

Expected: compilation fails because the module and types do not exist.

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

`canonical_fingerprint` returns lowercase SHA-256 hex. `physical_metrics` scans `World::cells()` in YZX order, ignores `BlockKind::Air`, uses inclusive extents, and uses denominator `max(lowered_gate_count, 1)`.

- [ ] **Step 4: Run focused and world regressions**

```powershell
cargo test --lib compile::metrics::tests -- --nocapture
cargo test --lib redstone::world -- --nocapture
```

Expected: all pass.

- [ ] **Step 5: Commit Task 0**

```powershell
git add Cargo.toml Cargo.lock src/compile/mod.rs src/compile/metrics.rs
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
- Records build/profile revisions, input hashes, fixed transition manifest hash, world fingerprint, physical metrics, and observed settle ticks.

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

Expected: compilation or fixture-open failure.

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

### Task 2: Introduce typed instance identity and pure topology instantiation

**Files:**
- Create: `src/compile/fragment_synth/identity.rs`
- Create: `src/compile/fragment_synth/topology.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/topology.rs`
- Modify: `src/compile/primitive_graph.rs`
- Test: `src/compile/fragment_synth/topology.rs` (`tests` module)

**Interfaces:**
- Produces `GateIndex`, `PortId`, `LibraryEntryId`, `InstanceId`, `TopologyNodeId`, `PrimitiveId`, `ConnectionId`, `RouteId`, `RoutedSinkId`, `PhysicalEndpointId`, `ImplementationKey`, `ExpandedInstance`, and `ValidatedTopology`.
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
git add src/compile/mod.rs src/compile/topology.rs src/compile/fragment_synth
git commit -m "feat(synthesis): add validated topology instances"
```

---

### Task 3: Build the one-to-one `InstanceGraph` and complete candidate state

**Files:**
- Create: `src/compile/fragment_synth/instance_graph.rs`
- Create: `src/compile/fragment_synth/candidate.rs`
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

`PrimitivePlacement` carries the selected physical variant, anchor, and full emitted `BlockState` data. `ConnectionBinding` names the typed source, landing, route tree, and concrete routed sink. `RealisedRouteTree` owns a shared trunk once, ordered branches, full route/floor `BlockState`s, and one terminal record per `RoutedSinkId`; fanout branches must not duplicate their common trunk in candidate identity or block metrics. Canonical gate positions/facings are derived compatibility views only.

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
- Create: `src/compile/fragment_synth/realise.rs`
- Create: `src/compile/fragment_synth/verify.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/planner.rs`
- Modify: `src/compile/equivalence.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Test: `src/compile/fragment_synth/verify.rs` (`tests` module)

**Interfaces:**
- Produces `realise_and_verify_expanded(candidate, netlist, library) -> Result<CertifiedWorld, CertificationError>`.
- Reuses existing collision, coupling, strength, direction, connectivity, merge, and terminal-rule implementations through a common lossless physical view.

- [ ] **Step 1: Write the required non-ignored corruption matrix**

Start from one certified candidate containing a two-node BUF and mixed merge. Clone and independently mutate it: remove an internal connection, redirect an internal connection to the wrong sink, remove and duplicate a primitive, change a primitive kind, reverse a primitive facing, add an extra primitive, mismatch implementation key/topology, reassign one external sink, omit a junction contributor, swap junction logical owner/contributor, delete a declared-output terminal, and falsify an observation point. Require a named `StructuralMismatch` carrying the affected stable ID before functional simulation starts.

- [ ] **Step 2: Run the corruption tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::verify::tests -- --nocapture
```

Expected: compilation fails because expanded certification does not exist.

- [ ] **Step 3: Extract a common physical verification view**

Refactor the body of old `planner::realise_and_verify` and the `compile/mod.rs` net/source plus collision/connectivity/merge/terminal invariant inputs so both old `PlanCandidate` and new `ExpandedPhysicalCandidate` provide the same complete verifier input. Do not convert typed endpoints to gate-name strings. The common view must retain shared route trees, internal routes, route terminal block states, duplicate identities, junction contributors, observations, and pins. The emitter and physical-rule implementations remain single shared authorities; `fragment_synth::realise` is an adapter, not a second emitter.

- [ ] **Step 4: Re-instantiate and compare before emission**

For every instance, call `instantiate(library, logical_gate, instance.id, &instance.implementation)` and compare exact primitive, connection, output, and fingerprint sets to candidate state. Then prove each expected junction contributor reaches exactly one verified observation and no unlisted contributor joins it. Only after those checks emit the world and run the reused physical rules.

- [ ] **Step 5: Run corruption, coupling, strength, and terminal suites**

```powershell
cargo test --lib compile::fragment_synth::verify::tests -- --nocapture
cargo test --test primitive_graph_equivalence -- --nocapture
cargo test --test terminal_handover -- --nocapture
cargo test --test or_merge -- --nocapture
cargo test --lib compile::coupling -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 4**

```powershell
git add src/compile/fragment_synth src/compile/mod.rs src/compile/planner.rs src/compile/equivalence.rs
git commit -m "feat(synthesis): certify expanded topology independently"
```

---

### Task 5: Preserve repeater orientation end to end

**Files:**
- Modify: `src/compile/fragment_synth/candidate.rs`
- Modify: `src/compile/fragment_synth/realise.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/fragment_synth/realise.rs` (`tests` module)
- Test: `tests/delay_model_reconciliation.rs`

**Interfaces:**
- Produces one route-step legality function that consumes full `BlockState` and is shared by routing and verification.
- Guarantees a repeater may be entered only from its rear and left only through its front, based on `BlockState::facing`.

- [ ] **Step 1: Write a failing axis-sensitive route test**

Build two otherwise identical candidate routes. One repeater faces along the predecessor-to-successor axis; the other is rotated 90 degrees. Require the first to certify and the second to return `WrongRepeaterAxis` naming the `ConnectionId` and anchor. Add a round-trip test asserting `kind`, `facing`, and `delay` survive route proposal, candidate storage, emission, and verification.

- [ ] **Step 2: Run the focused tests**

```powershell
cargo test --lib compile::fragment_synth::realise::tests repeater -- --nocapture
```

Expected: the rotated fixture is rejected and full-state round-trip passes only after the common path uses `BlockState`, not `BlockKind`.

- [ ] **Step 3: Centralise full-state conduction checks**

Implement:

```rust
fn route_step_is_legal(previous: Anchor, at: Anchor, next: Anchor, state: &BlockState) -> bool;
```

Use the same function in StrengthAware search, route reconstruction, expanded emission, and structural verification. Remove any route-state reconstruction that defaults repeater facing or delay.

- [ ] **Step 4: Run routing and delay reconciliation**

```powershell
cargo test --lib compile::fragment_synth::realise::tests -- --nocapture
cargo test --test delay_model_reconciliation terminal_repeaters_are_every_repeater_on_the_path -- --exact --nocapture
cargo test --test compile_end_to_end -- --nocapture
```

Expected: all pass.

- [ ] **Step 5: Commit Task 5**

```powershell
git add src/compile/fragment_synth src/compile/planner.rs tests/delay_model_reconciliation.rs
git commit -m "fix(routing): retain repeater state through certification"
```

---

### Task 6: Derive the primitive-level realised timing graph

**Files:**
- Create: `src/compile/fragment_synth/timing_graph.rs`
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/timing/mod.rs`
- Modify: `src/redstone/simulator/mod.rs`
- Modify: `src/redstone/simulator/observer.rs`
- Test: `src/compile/fragment_synth/timing_graph.rs` (`tests` module)
- Test: `tests/delay_model_reconciliation.rs`

**Interfaces:**
- Produces `TimingNodeId`, `TimingArcId`, `TimingArcKind`, `ExactDelay`, `RealisedTimingGraph`, `StaticTiming`, typed observations, and `TransitionWitness`.
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

Add typed observer registration through `Simulator::attach_observer`, raw observation events, and a typed transition result so timelines are keyed by `ObservationId`, with human labels as metadata. Store observation sites as `Position -> Vec<ObservationSite>` so an ordinary instance output may alias its selected primitive at the same coordinate without either identity disappearing. Keep the existing string-labelled timing API as a compatibility wrapper rather than collapsing typed identities. Extend every `CompiledCircuit` constructor with a `CircuitObservations` value containing primitive, concrete instance, and junction maps keyed by typed IDs; retain the current gate-output map as the canonical compatibility projection. Retain every transition index tied for worst settle time, while keeping the lowest index as the compatibility scalar. Two duplicates with the same display label, and two typed identities at the same position, must retain independent event sequences.

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

### Task 7: Add immutable manifests and complete functional certification

**Files:**
- Modify: `src/compile/fragment_synth/manifest.rs`
- Create: `src/compile/fragment_synth/certification.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/equivalence.rs`
- Test: `src/compile/fragment_synth/manifest.rs` and `certification.rs` (`tests` modules)

**Interfaces:**
- Produces `TransitionManifest`, `CertificationConfig`, `CertifiedCandidate`, `CandidateMetrics`, and fresh-simulator transition sweeps.
- Promotes a candidate only after structural, functional, and complete manifest certification.

- [ ] **Step 1: Write exact manifest tests**

For 0-4 inputs, assert every ordered pair of distinct vectors and exactly 240 transitions for four inputs. Above four inputs, require all-zero, all-one, one-hot, one-cold, and every single-bit toggle among those vectors. Assert deterministic order and hash independence from candidate data.

- [ ] **Step 2: Write fresh-simulator and functional-gate tests**

Create a circuit whose shared simulator historically retains an event but whose fresh simulator settles correctly. Require each manifest entry to clone the certified world, apply source as a batch, settle, record `start_tick`, apply destination as a batch, and measure quiescence. Require divergence to return `TransitionDidNotSettle`, not a capped score.

For up to eight inputs, exhaustively compare outputs. Above eight inputs, require a successful instance-aware equivalence proof; an unavailable or failed proof cannot produce `CertifiedCandidate`.

- [ ] **Step 3: Run tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::manifest::tests -- --nocapture
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
```

Expected: compilation fails because certification types do not exist.

- [ ] **Step 4: Implement certification and metrics**

Define the lexicographic quality key:

```rust
pub struct QualityKey {
    pub observed_settle: u64,
    pub non_air_blocks: u64,
    pub occupied_volume: u64,
    pub static_routed_delay: ExactDelay,
}
```

Record manifest hash, count, cap, all worst transition indexes, realised timing graph fingerprint, canonical candidate fingerprint, and emitted-world fingerprint. Candidate fingerprints only break complete quality ties and never count as improvement.

- [ ] **Step 5: Run certification and existing truth-table regressions**

```powershell
cargo test --lib compile::fragment_synth::certification::tests -- --nocapture
cargo test --test compile_end_to_end -- --nocapture
cargo test --test reference_circuits -- --nocapture
cargo test --test seven_segment -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit Task 7**

```powershell
git add src/compile/fragment_synth src/compile/equivalence.rs
git commit -m "feat(synthesis): certify fixed transition workloads"
```

---

### Task 8: Implement the independent topology-aware sparse seed

**Files:**
- Create: `src/compile/fragment_synth/seed.rs`
- Modify: `src/compile/fragment_synth/mod.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/fragment_synth/seed.rs` (`tests` module)
- Test: `tests/build_circuit_pins.rs`

**Interfaces:**
- Produces `SparseSeedBuilder::build(SynthesisInput) -> Result<CertifiedCandidate, SeedError>`.
- May call shared routing primitives and expanded realisation/certification, but not an old generator or old placement policy.

- [ ] **Step 1: Write a source-boundary guard test**

Add a test that reads `src/compile/fragment_synth/seed.rs` and rejects references to `compile_legacy`, `compile_planned`, `compile_grown`, `seed_from_legacy`, `plan_from_netlist`, `starting_layout`, `relax`, or old `optimise`. This is an architectural regression test, not the correctness proof.

- [ ] **Step 2: Write deterministic seed tests**

For NOT, and4, fanout, two-node BUF, bare merge, mixed merge, fully isolated merge, and pinned and4, build twice and require byte-identical candidate/world fingerprints, full certification, exact pin coordinates, and a complete `InstanceGraph`. Add a stateful rejection test before any placement and a named bounded exhaustion test carrying instance, primitive, and radius.

- [ ] **Step 3: Run focused tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
```

Expected: compilation fails because the independent builder does not exist.

- [ ] **Step 4: Implement deterministic ordering and placement**

Process canonical instances in stable topological order, primitives in `TopologyNodeId` order, facings in fixed NESW order, physical variants by library order, and anchors in expanding Manhattan shells around deterministic barycentres of already placed sources/sinks. Place pinned handover primitives first and never move their caller cells.

Every trial checks physical-variant footprint and keep-out constraints locally. If the completed candidate fails authoritative certification, backtrack deterministically to the next placement choice; report the first stable refusal identity when the configured seed radius is exhausted.

- [ ] **Step 5: Implement topology-aware routing**

Route external and internal `ConnectionId`s in stable critical-estimate/fanout/ID order using the existing StrengthAware physical search. Internal edges are ordinary routing obligations. Materialise junction contributors explicitly. After all routes exist, call `realise_and_verify_expanded` and full certification; only that result is the seed.

- [ ] **Step 6: Prove zero-budget behaviour and run pin regressions**

Expose a temporary `compile_sparse_seed` test helper and require it to return the certified seed without starting optimisation.

```powershell
cargo test --lib compile::fragment_synth::seed::tests -- --nocapture
cargo test --test build_circuit_pins -- --nocapture
cargo test --test terminal_handover -- --nocapture
```

Expected: all pass.

- [ ] **Step 7: Commit Task 8**

```powershell
git add src/compile/fragment_synth src/compile/planner.rs tests/build_circuit_pins.rs
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
- Produces `SynthesisInput`, `SynthesisBudget`, `SearchConfig`, `SynthesisResult`, `StopReason`, `ProposalTrace`, and explicit `compile_fragment_synth`.
- Initially enumerates deterministic no-op/refused proposals so budget semantics land before search policy.

- [ ] **Step 1: Write budget-prefix and best-retention tests**

Run fresh syntheses at evaluation budgets 0, 1, 2, 4, and 8. Require budget 0 to return the certified seed; traces at smaller budgets to be byte-identical prefixes of larger traces; completed evaluations never exceed budget; and final quality never worsen. Inject refused, verification-failed, and certification-failed proposals and require none to erase `best_certified`.

- [ ] **Step 2: Write time-budget equivalence tests around a fake clock**

Use an injected monotonic clock in unit tests. If time mode starts and finishes `k` proposals, require its trace/result to equal `Evaluations(k)`. Advance the fake deadline during a transaction and require that proposal to finish before `StopReason::TimeBudget` is returned.

- [ ] **Step 3: Run tests and verify RED**

```powershell
cargo test --lib compile::fragment_synth::search::tests -- --nocapture
```

Expected: compilation fails because the state machine does not exist.

- [ ] **Step 4: Implement the loop-boundary budget state machine**

Compute `SynthesisCaseFingerprint` from lowered netlist and port order, pins, library revision, search/certification config, simulator/verifier revisions, and manifest hash. Read the requested budget only at the loop boundary. Record proposal index, parent fingerprint, fragment ID, choice fingerprint, terminal outcome, certified quality, and accepted flag.

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

Keep `compile`, `compile_grown`, CLI, baker, and viewer unchanged. Add `PlannerKind::FragmentSynth` only to results from this explicit API.

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

Rank zero-slack/low-slack arcs, delayed primitive events, glitches, and worst targets using complete tied-worst witnesses. Expand to bounded instance plus route-tree closure. Enumerate implementation choices, facings, variants, bounded anchor shells, and reroute choices in stable typed-ID order. The fragment and choice canonical encodings form their fingerprints.

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
cargo test --lib compile::fragment_synth::fragment::tests duplication -- --nocapture
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
- Modify: `tests/fixtures/fragment_synth_baseline.json` only if its schema gains fields, never its recorded baseline values

**Interfaces:**
- Produces a machine-readable acceptance report and a checked shipping evaluation budget only when every spec condition passes.
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

Run 0, 1, 2, 4, 8, and candidate shipping budgets in shuffled order, each in at least three fresh processes. Compare trace, metrics, candidate fingerprint, and emitted-world fingerprint byte-for-byte while excluding elapsed time. Require used evaluations <= budget and non-worsening quality.

- [ ] **Step 4: Run the harness from a clean checkout**

```powershell
git status --short
cargo test --release --test fragment_synth_acceptance -- --nocapture
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output tests/fixtures/fragment_synth_shipping.json
```

Expected: `git status --short` is empty before the run; the report explicitly says either `replacement_gate_passed: true` with the smallest passing checked evaluation budget, or `false` with named failing cases/conditions.

- [ ] **Step 5: Commit only measured acceptance data**

If the gate passes, commit the report and fixed shipping budget. If it fails, commit the harness and failure report but do not create a shipping budget or alter any front door.

```powershell
git add src/compile/fragment_synth/benchmark.rs src/bin/fragment_baseline.rs src/bin/fragment_acceptance.rs tests/fragment_synth_acceptance.rs tests/fixtures/fragment_synth_shipping.json
git commit -m "test(synthesis): enforce the replacement gate"
```

---

### Task 13: Conditionally switch front doors, regenerate artifacts, and delete legacy generation

**Precondition:** `tests/fixtures/fragment_synth_shipping.json` exists, says `replacement_gate_passed: true`, records the smallest passing fixed evaluation budget, and Task 12 passes from a clean checkout. If any condition is false, stop this plan after Task 12 with the explicit new API intact.

**Files:**
- Modify: `src/compile/mod.rs`
- Modify: `src/bin/build_circuit.rs`
- Modify: `src/bin/mc_dump.rs`
- Modify: `viewer/src/lib.rs`
- Modify: `viewer/tests/*.rs` where planner-kind expectations change
- Modify: checked-in baked `.litematic` and pinout sidecars produced by `build_circuit`
- Delete: legacy generator/fallback code only in a separate commit after switchover verification
- Modify: tests and reports that intentionally target deleted legacy APIs

**Interfaces:**
- `compile(&Netlist)` calls fragment synthesis with no pins and the fixed shipping evaluation budget.
- `compile_grown(&Netlist, &PortPlacements)` becomes a compatibility wrapper over the same API.
- `compile_fragment_synth` remains the advanced result/trace API.
- `PlannerKind` exposes only shipping fragment synthesis as a public generation policy after deletion.

- [ ] **Step 1: Assert the precondition in code and tests**

Parse the checked shipping report at build/test time and require the exact synthesis-case fingerprint inputs and fixed budget. Do not choose a budget dynamically.

- [ ] **Step 2: Switch library, CLI, baker, and viewer callers**

Route all checked artifacts through `compile_fragment_synth(SynthesisInput { lowered: &netlist, source_provenance: provenance.as_deref(), pins: Some(&placements) }, Evaluations(SHIPPING_EVALUATIONS))`. Preserve already-lowered input semantics, supplied pins, structured errors, and caller-owned pin cells. Update viewer annotations so one logical gate may list multiple instance outputs while the canonical instance remains the compatibility position.

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
git add src/compile/mod.rs src/bin viewer tests
git add -u
git commit -m "feat(compile): ship timing-directed fragment synthesis"
```

- [ ] **Step 5: Delete legacy generation and fallback code**

Remove `compile_legacy`, old planner-selection fallback, old placement/optimisation policy, and tests/reports that exist only to select those policies. Retain shared routing physics, emitter, verifier rules, simulator, topology library, and any diagnostic API still used by the new synthesiser. Do not combine this deletion with the front-door switch commit.

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
- [ ] Search all new files for unfinished placeholder markers and resolve every hit before execution is called complete.
- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets`.
- [ ] Confirm every acceptance test is non-ignored, every candidate promoted to best owns a certified world, and every rejected proposal leaves the parent fingerprint unchanged.
- [ ] Confirm clean-worktree Task 12 evidence exists before performing any Task 13 deletion.
