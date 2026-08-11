# Unified 3D Planner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the row/channel/track architecture with a deterministic,
weighted, joint 3D placement-and-routing candidate model that can first seed
from the legacy emitter and then improve it.

**Architecture:** The primitive graph remains signal-only.  A new planner
module owns anchors, physical primitive variants, routes and a derived
reservation map as one legal `PlanCandidate`.  The old compiler produces the
first candidate only during migration; every subsequent candidate move both
places and reroutes its affected signal edges.

**Tech Stack:** Rust 2021, existing `PrimitiveGraph`, `World`, compiler
invariants, simulator timing model and `./check.sh`.

## Global Constraints

- Minecraft Java **26.2** is the only product target; 1.20.1 evidence is
  historical only.
- `PrimitiveGraph` models signal primitives, not dust or support blocks.
- Dust/support/orientation/repeater choice are physical realisation decisions.
- Spacing, connectivity, torch merge and signal strength are hard constraints.
- All planning is reproducible from `PlannerSeed`, `PlannerWeights` and
  `PlannerEffort`.
- The current row/channel/track path may seed the new model but must not make
  final optimisation decisions.
- No editor work belongs to this plan.

---

### Task 1: Planner data model and normalised cost

**Files:**
- Create: `src/compile/planner.rs`
- Modify: `src/compile/mod.rs`
- Test: `src/compile/planner.rs`

**Interfaces:**
- Produces `pub struct Anchor { pub x: i32, pub y: i32, pub z: i32 }`.
- Produces `pub struct PlannerWeights { pub delay: u32, pub wire: u32, pub space: u32, pub turns: u32 }`.
- Produces `pub struct PlannerEffort { pub evaluations: usize, pub seed: u64 }`.
- Produces `pub struct CostBreakdown { pub delay: u64, pub wire: u64, pub space: u64, pub turns: u64 }` and a total normalised score.

- [x] **Step 1: Write failing cost tests**

```rust
#[test]
fn a_seed_scores_one_for_every_nonzero_normalised_term() {
    let seed = fixture_seed();
    assert_eq!(seed.score(&PlannerWeights::default()), NormalisedScore::ONE);
}

#[test]
fn same_candidate_weights_effort_and_seed_score_identically() {
    assert_eq!(run_fixture(17), run_fixture(17));
}
```

- [x] **Step 2: Run the focused tests**

Run: `cargo test --lib planner::tests`

Expected: compile failure because `planner` and its public types do not exist.

- [x] **Step 3: Implement immutable candidate metadata and scoring**

`PlanCandidate` initially contains anchors and route identifiers only; it may
not call `World` mutation while scoring.  Use rational numerator/denominator
pairs or integer cross multiplication for normalisation, never floating-point
ordering.  Derive `space` from occupied bounding volume and `turns` from
nonterminal route turns.  Return an ordered score tuple so a tie is stable.

- [x] **Step 4: Run the planner and full compiler tests**

Run: `cargo test --lib planner::tests`; `cargo test --release`

Expected: all pass; no current output changes.

- [x] **Step 5: Commit**

```bash
git add src/compile/planner.rs src/compile/mod.rs
git commit -m "feat(planner): add deterministic candidate cost model"
```

### Task 2: Physical primitive variants and typed ports

**Files:**
- Create: `src/compile/physical.rs`
- Modify: `src/compile/planner.rs`
- Test: `src/compile/physical.rs`

**Interfaces:**
- Produces `pub enum PortKind { TorchInput, TorchOutput, RepeaterRear, RepeaterSide, RepeaterFront, ComparatorRear, ComparatorSide, ComparatorFront, LeverOutput, LampInput }`.
- Produces `pub enum RelativeSide { Left, Right }` and `PhysicalPort { kind, side: Option<RelativeSide>, .. }`, so the public electrical kind remains `RepeaterSide`/`ComparatorSide` while the two physical side locations remain unambiguous.
- Produces `pub struct PhysicalVariant` with every local block required for placement (including support) and typed local ports.
- Produces `pub fn variants(primitive: Primitive) -> &'static [PhysicalVariant]`.

- [x] **Step 1: Write failing orientation/port tests**

```rust
#[test]
fn a_torch_variant_exposes_input_on_its_support_and_output_at_its_torch() {
    let variant = variants(Primitive::Torch)[0];
    assert!(variant.ports.iter().any(|port| port.kind == PortKind::TorchInput));
    assert!(variant.ports.iter().any(|port| port.kind == PortKind::TorchOutput));
}

#[test]
fn repeater_rear_and_front_ports_are_opposite_and_side_ports_are_orthogonal() {
    let variant = variants(Primitive::Repeater)[0];
    assert_eq!(variant.port(PortKind::RepeaterRear).direction.opposite(), variant.port(PortKind::RepeaterFront).direction);
    let sides = variant.ports_of(PortKind::RepeaterSide);
    assert_eq!([sides[0].side, sides[1].side], [Some(RelativeSide::Left), Some(RelativeSide::Right)]);
    assert!(variant.port(PortKind::RepeaterRear).direction.is_orthogonal_to(sides[0].direction));
}
```

- [x] **Step 2: Run the focused tests**

Run: `cargo test --lib physical::tests`

Expected: compile failure because `physical` does not exist.

- [x] **Step 3: Implement only the variants current circuits need**

Implement oriented Torch, Repeater, Lever and Lamp variants using the same
Minecraft facing convention already verified by component tests.  Do not add
coordinates to `topology::Template` or `PrimitiveGraph`.

- [x] **Step 4: Run physical, simulator and full tests**

Run: `cargo test --lib physical::tests`; `cargo test --release`

Expected: all pass.

- [x] **Step 5: Commit**

```bash
git add src/compile/physical.rs src/compile/planner.rs
git commit -m "feat(planner): describe physical primitive ports"
```

### Task 3: Extract the legacy compiler result as a legal seed

**Files:**
- Modify: `src/compile/mod.rs`
- Modify: `src/compile/planner.rs`
- Test: `tests/compile_end_to_end.rs`

**Interfaces:**
- Produces `pub fn seed_from_legacy(netlist: &Netlist, compiled: &CompiledCircuit) -> Result<PlanCandidate, PlannerError>`.
- Produces `pub fn verify_candidate(candidate: &PlanCandidate) -> Result<(), PlannerError>`.

- [x] **Step 1: Write failing seed equivalence tests**

```rust
#[test]
fn legacy_and4_extracts_to_a_legal_candidate_with_unit_seed_score() {
    let (netlist, compiled) = compiled_and4();
    let seed = seed_from_legacy(&netlist, &compiled).unwrap();
    verify_candidate(&seed).unwrap();
    assert_eq!(seed.score(&PlannerWeights::default()), NormalisedScore::ONE);
}

#[test]
fn extracted_candidate_preserves_each_primitive_anchor_and_route_owner() {
    let (netlist, compiled) = compiled_and4();
    let seed = seed_from_legacy(&netlist, &compiled).unwrap();
    assert!(seed.anchors.iter().all(Option::is_some));
    assert!(seed.routes.iter().all(|route| route.owner.is_some()));
}
```

- [x] **Step 2: Run the focused tests**

Run: `cargo test --test compile_end_to_end legacy_and4_extracts`

Expected: compile failure because extraction does not exist.

- [x] **Step 3: Emit enough ownership metadata for extraction**

Record primitive anchors, route ownership and terminal kind while the legacy
emitter already knows them.  Do not reconstruct ownership by scanning block
colours.  Have the seed verifier invoke the same four existing physical
invariants against its realised world.

- [x] **Step 4: Verify all reference circuits**

Run: `cargo test --release --test reference_circuits`; `cargo test --release --test verilog_frontend`; `./check.sh`

Expected: each legacy output extracts, verifies and retains its existing
truth-table result.

- [x] **Step 5: Commit**

```bash
git add src/compile/mod.rs src/compile/planner.rs tests/compile_end_to_end.rs
git commit -m "feat(planner): seed legal candidates from legacy routing"
```

### Task 4: Joint local move, incremental rip-up and directed terminal choice

**Files:**
- Modify: `src/compile/planner.rs`
- Modify: `src/compile/mod.rs`
- Test: `src/compile/planner.rs`
- Test: `tests/channel_safety.rs`

**Interfaces:**
- Produces `pub fn try_move(candidate: &PlanCandidate, primitive: NodeId, to: Anchor) -> Result<PlanCandidate, PlannerError>`.
- Produces `pub enum TerminalStyle { DirectedDustIntoSupport, RepeaterIntoSupport }`.

- [x] **Step 1: Write failing local-change tests**

```rust
#[test]
fn moving_one_anchor_reroutes_only_its_incident_edges() {
    let seed = fixture_seed();
    let moved = try_move(&seed, 3, Anchor { x: 4, y: 2, z: 7 }).unwrap();
    for edge in seed.non_incident_edges(3) {
        assert_eq!(seed.route(edge), moved.route(edge));
    }
}

#[test]
fn a_straight_directed_terminal_uses_dust_but_cornered_terminal_uses_repeater() {
    assert_eq!(terminal_style(&straight_support_approach()), TerminalStyle::DirectedDustIntoSupport);
    assert_eq!(terminal_style(&cornered_support_approach()), TerminalStyle::RepeaterIntoSupport);
}
```

- [x] **Step 2: Run the focused tests**

Run: `cargo test --lib moving_one_anchor straight_directed_terminal`

Expected: failure because no joint move or terminal selector exists.

- [x] **Step 3: Implement rip-up/reconnect against the candidate reservation**

Remove the moved primitive's local realisation and only routes incident to it.
Reserve the new variant, then route each incident edge with a deterministic
Manhattan/A* search that checks the live candidate reservation.  Test terminal
shape with the existing directed-dust predicate; use a repeater if direction,
strength or isolation is not proven.

- [x] **Step 4: Run safety and full tests**

Run: `cargo test --test channel_safety`; `cargo test --release`; `./check.sh`

Expected: all constraints continue to reject collisions; no false directed
dust terminal is emitted.

- [x] **Step 5: Commit**

```bash
git add src/compile/planner.rs src/compile/mod.rs tests/channel_safety.rs
git commit -m "feat(planner): jointly move primitives and reroute signals"
```

### Task 5: Deterministic anytime optimisation and topology feedback

**Files:**
- Modify: `src/compile/planner.rs`
- Modify: `src/compile/primitive_graph.rs`
- Modify: `src/compile/topology.rs`
- Test: `src/compile/planner.rs`
- Test: `tests/verilog_frontend.rs`

**Interfaces:**
- Produces `pub fn optimise(seed: PlanCandidate, weights: PlannerWeights, effort: PlannerEffort) -> PlanCandidate`.
- Produces `pub struct GateEffort` reporting each cell's local route/variant cost.

- [x] **Step 1: Write failing deterministic and feedback tests**

```rust
#[test]
fn fixed_seed_weights_and_effort_choose_the_same_legal_candidate() {
    let seed = fixture_seed();
    let effort = PlannerEffort { evaluations: 128, seed: 0x26_02 };
    assert_eq!(optimise(seed.clone(), PlannerWeights::default(), effort), optimise(seed, PlannerWeights::default(), effort));
}

#[test]
fn a_rejected_topology_alternative_leaves_the_best_candidate_unchanged() {
    let seed = fixture_seed_with_illegal_alternative();
    let result = optimise(seed.clone(), PlannerWeights::default(), PlannerEffort { evaluations: 128, seed: 1 });
    assert_eq!(result.selected_entry(2), seed.selected_entry(2));
}
```

- [x] **Step 2: Run the focused tests**

Run: `cargo test --lib fixed_seed_weights rejected_topology`

Expected: failure because optimisation is absent.

- [x] **Step 3: Implement stable best-first local search**

Enumerate node moves, orientation changes and terminal styles in stable id
order, evaluate no more than `effort.evaluations` legal candidates, and retain
the best lexicographic score.  At each epoch produce `GateEffort`; re-expand a
gate only when a library alternative is predicted to lower its measured local
cost, and accept it only if the whole candidate is legal and better.

- [x] **Step 4: Measure reference circuits**

Run: `cargo test --release --test reference_circuits -- --nocapture`; `cargo test --release --test verilog_frontend -- --nocapture`; `./check.sh`

Expected: truth tables pass.  Record exact blocks/ticks and retain the default
weights only when no reference circuit regresses on every measured primary
metric.

- [x] **Step 5: Commit**

```bash
git add src/compile/planner.rs src/compile/primitive_graph.rs src/compile/topology.rs tests/verilog_frontend.rs
git commit -m "feat(planner): optimise joint 3d layouts by effort budget"
```

### Task 6: Make the new planner the compiler path and document 26.2 evidence

**Files:**
- Modify: `src/compile/mod.rs`
- Modify: `docs/superpowers/specs/2026-08-11-unified-3d-planner.md`
- Modify: `README.md`
- Test: `tests/reference_circuits.rs`
- Test: `tests/verilog_frontend.rs`

- [x] **Step 1: Write a failing compiler-path assertion**

```rust
#[test]
fn compile_uses_the_planner_path_for_an_optimised_verilog_circuit() {
    let netlist = lower_optimised(&verilog::seven_segment().netlist).unwrap();
    let compiled = compile(&netlist).unwrap();
    assert_eq!(compiled.planner_kind(), PlannerKind::Unified3d);
}
```

- [x] **Step 2: Run the test**

Run: `cargo test --test verilog_frontend compile_uses_the_planner_path`

Expected: failure because `compile` still calls the row/channel/track emitter.

- [x] **Step 3: Switch `compile` to the planner and retain legacy only as a seed adapter**

The result must emit a `World` from `PlanCandidate`, run all invariants, and
expose planner diagnostics.  Remove the legacy path from ordinary compilation
only after its seed extraction is covered by tests.

- [x] **Step 4: Verify format and target runtime boundary**

Run: `./check.sh`; `cargo test --release --test verilog_litematic`; `cargo run --release --bin build_circuit -- verilog:seven_segment`; start the 26.2 server and perform component probes/readback as documented.

Expected: Litematic declares 26.2 schema/data version and static 26.2
component probes pass.  Dynamic diode/sequential proof remains a real-client
action and is recorded separately.

- [x] **Step 5: Commit**

```bash
git add src/compile/mod.rs docs/superpowers/specs/2026-08-11-unified-3d-planner.md README.md tests/reference_circuits.rs tests/verilog_frontend.rs
git commit -m "feat(compile): use the unified 3d planner"
```

## Plan self-review

- Every candidate contains positions and routes together; no task reinstates a
  placement-then-router boundary.
- Topology stays coordinate-free and physical ports are a separate layer.
- Cost terms are normalised and incremental; legality is never a weighted
  penalty.
- Directed dust is a selectable, validated terminal style rather than an
  emitter exception.
- The tasks defer UI/editor work and require 26.2 artefacts and evidence.

## Status (2026-08-12)

All six tasks are implemented. `compile()` ships the world the planner
realises, `CompiledCircuit::planner_kind()` reports `Unified3d`, and every
reference circuit is byte-identical to what the legacy emitter produced --
which is the point: the switch changed nothing observable.

Two things this plan did not anticipate, both now done:

- `compile_planned` places from the netlist alone, with no legacy seed. The
  plan treated legacy seeding as permanent scaffolding; it is now one of two
  entry points.
- Port positions are an input that defaults to empty (`PortPlacements`). The
  plan assumed the emitter's port placement throughout.

What the plan promised and did not get: `optimise` improves and4 (472 blocks
and 18 ticks become 405 and 16) and effectively nothing larger, because its
move set is six one-cell translations against a layout the row/channel router
already packed. And placement from the netlist alone carries as far as and4;
`how_far_the_planners_own_placement_carries` records why.
