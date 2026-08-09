# Global Polarity Assignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lower a gate-level netlist using globally chosen output polarities, so REDA eliminates cross-cell inversions without giving ABC technology mapping authority again.

**Architecture:** `topology` owns truth-table-checked expansions for both representations of each logical `GateKind`. Each recipe also declares the polarity required at each input port: an AND-shaped torch can consume `!a`/`!b` directly, rather than first recreating those inversions locally. `lowering` remains mechanical: it resolves each requested signed input from the chosen physical representation of its producer, adding one shared inverter only when the required rail does not exist. A deterministic whole-netlist search evaluates complete candidates, not independent gate costs. Placement, routing, and all four correctness invariants remain unchanged.

**Tech Stack:** Rust, existing `GateKind`/`Expansion` topology library, `NetlistBuilder`, Rust unit and integration tests, existing RCON conformance harness.

## Global Constraints

- The gate-level netlist from Yosys must remain unchanged: its kind histogram is pinned by `the_baked_seven_segment_is_a_gate_level_circuit_not_a_wall_of_nors`.
- `lower` remains the identity on already-realisable hand-written NOR/OR netlists.
- The four existing legality checks — spacing, connectivity, torch merge, signal strength — remain mandatory hard constraints, never weighted costs.
- The control circuits remain byte-for-byte at their measured values: and4 472/24, full_adder 1784/62, segment_a 6416/82, seven_segment 16244/112.
- The Verilog decoder's target is at least the old mapped baseline: 31 placed gates, 7,888 blocks, 82 ticks; every reduced result must pass simulator truth tables and the real-Minecraft RCON sequence.
- No genlib is reintroduced and ABC stays logic-only; no NOR-cluster pattern reconstruction is allowed.

---

## File Structure

- `src/compile/topology.rs`: declares signal polarity, stores/evaluates signed-input expansions, and prices each expansion.
- `src/compile/polarity.rs`: assigns a polarity to every gate in a gate-level DAG and exposes a deterministic `PolarityAssignment`.
- `src/compile/lowering.rs`: lowers with an assignment while preserving today's `lower()` as the ordinary-polarity compatibility wrapper.
- `src/compile/mod.rs`: exports `polarity` from the compile module.
- `tests/verilog_frontend.rs`: measures the end-to-end decoder only after the assignment pass is enabled.
- `conformance/circuit_conformance.py`: unchanged unless the existing generic harness exposes a missing observation; use it only for final real-game verification.

### Task 1: Signed topology recipes

**Files:**
- Modify: `src/compile/topology.rs:730-1125`
- Modify: `src/compile/lowering.rs:145-151` (mechanical exhaustive-match migration only)
- Test: `src/compile/topology.rs` test module

**Interfaces:**
- Produces `pub enum SignalPolarity { Positive, Negative }`.
- Replaces `Operand::Input(usize)` with `Operand::Input { pin: usize, polarity: SignalPolarity }`.
- Produces `pub fn expansion_for_polarity(kind: GateKind, polarity: SignalPolarity) -> Expansion`.
- Produces `pub fn expansion_cost_for_polarity(kind: GateKind, polarity: SignalPolarity) -> RealisationCost`.
- Keeps `expansion_for(kind)` and `expansion_cost(kind)` as `Positive` compatibility wrappers.

- [ ] **Step 1: Write failing truth-table and cost tests**

```rust
#[test]
fn every_negative_expansion_computes_the_complement_of_its_gate() {
    for kind in every_gate_kind() {
        for bits in 0..(1u32 << kind.arity()) {
            let inputs = bits_to_inputs(kind.arity(), bits);
            assert_eq!(
                expansion_for_polarity(kind, SignalPolarity::Negative).evaluate(&inputs),
                !kind.evaluate(&inputs),
            );
        }
    }
}

#[test]
fn nand_is_the_negative_realisation_of_and() {
    assert_eq!(
        expansion_cost_for_polarity(GateKind::And, SignalPolarity::Negative),
        expansion_cost(GateKind::Nand),
    );
}
```

- [ ] **Step 2: Run the two tests and verify they fail because the API does not exist**

Run: `cargo test --lib negative_expansion nand_is_the_negative_realisation_of_and`

Expected: compilation failure naming `SignalPolarity` and `expansion_for_polarity`.

- [ ] **Step 3: Implement the polarity-aware recipe API**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalPolarity { Positive, Negative }

pub fn expansion_for_polarity(kind: GateKind, polarity: SignalPolarity) -> Expansion {
    match polarity {
        SignalPolarity::Positive => positive_expansion_for(kind),
        SignalPolarity::Negative => negative_expansion_for(kind),
    }
}
```

Normalise the existing recipes so their external operands state what they actually require. For example, positive AND becomes `Nor([Input{pin: 0, polarity: Negative}, Input{pin: 1, polarity: Negative}])`; negative AND is the same inputs finished by `Merge`. Do the same for every supported kind, including asymmetric pin order for `AndNot` and `Mux`. Do not append a generic final inverter: the selected recipe itself must represent the other polarity. Make `expansion_for` call the positive branch and derive cost by walking the selected expansion exactly as today's `expansion_cost` does. Update `lowering.rs` only enough to compile against the new enum shape; resolving a negative requested input rail remains Task 2.

- [ ] **Step 4: Run topology unit tests and the full root test suite**

Run: `cargo test --lib topology::tests` and `cargo test --release`

Expected: all tests pass; positive recipes and all reference-circuit measurements are unchanged.

- [ ] **Step 5: Commit the isolated topology capability**

```bash
git add src/compile/topology.rs
git commit -m "feat(topology): add complemented gate expansions"
```

### Task 2: Assignment-aware lowering without optimisation

**Files:**
- Modify: `src/compile/lowering.rs:90-180`
- Modify: `src/compile/mod.rs` module declarations
- Test: `src/compile/lowering.rs` test module

**Interfaces:**
- Produces `pub type PolarityAssignment = Vec<SignalPolarity>` in `src/compile/polarity.rs`.
- Produces `pub fn lower_with_assignment(netlist: &Netlist, assignment: &[SignalPolarity]) -> Result<Netlist, LowerError>`.
- Keeps `lower(netlist)` exactly equivalent to passing `Positive` for every gate.

- [ ] **Step 1: Write failing lowering tests**

```rust
#[test]
fn assigned_negative_and_still_exports_the_and_function() {
    let source = netlist(&["a", "b"], &["y"], vec![gate(GateKind::And, "y", &["a", "b"])]);
    let lowered = lower_with_assignment(&source, &[SignalPolarity::Negative]).unwrap();
    assert_eq!(evaluate(&lowered, &[("a", true), ("b", true)])["y"], true);
}

#[test]
fn all_positive_assignment_is_identical_to_lower() {
    let source = representative_gate_level_netlist();
    assert_eq!(
        lower_with_assignment(&source, &vec![SignalPolarity::Positive; source.gates.len()]).unwrap(),
        lower(&source).unwrap(),
    );
}
```

- [ ] **Step 2: Run these tests and verify the new lowering API is absent**

Run: `cargo test --lib assigned_negative_and all_positive_assignment`

Expected: compilation failure naming `lower_with_assignment`.

- [ ] **Step 3: Add `polarity.rs` and implement mechanical assigned lowering**

```rust
pub type PolarityAssignment = Vec<SignalPolarity>;

pub fn lower_with_assignment(
    netlist: &Netlist,
    assignment: &[SignalPolarity],
) -> Result<Netlist, LowerError> {
    // validate assignment length, resolve every logical input at the signed
    // polarity its selected recipe requests, and select the recipe whose
    // output has assignment[source]'s physical polarity.
}
```

Maintain a `LogicalSignal -> PhysicalRail { positive, negative }` map while walking the topological order. A primary input starts with only its positive rail. For a requested missing rail, call the existing cached `NetlistBuilder::not` once and register its result. For a gate selected negative, give its physical result a fresh generated signal name; if its original output is a declared circuit output, emit the one required final inverter under the original output name. This preserves the observable boolean function while allowing interior consumers to use either rail. Reuse the existing cache for every requested inverse. Do not allow a negative polarity for already-realisable source gates in this task; return `LowerError::UnsupportedAssignedPolarity` so the hand-written control group cannot move accidentally.

- [ ] **Step 4: Run lowering, reference-circuit, and Verilog frontend tests**

Run: `cargo test --lib lowering::tests`; `cargo test --release --test reference_circuits`; `cargo test --release --test verilog_frontend`

Expected: all pass; calling `lower()` retains current 56 / 12,348 / 88 output.

- [ ] **Step 5: Commit assignment-aware lowering**

```bash
git add src/compile/mod.rs src/compile/polarity.rs src/compile/lowering.rs
git commit -m "feat(lowering): accept explicit gate output polarities"
```

### Task 3: Deterministic global polarity assignment

**Files:**
- Modify: `src/compile/polarity.rs`
- Modify: `src/compile/lowering.rs`
- Test: `src/compile/polarity.rs` test module
- Test: `tests/verilog_frontend.rs`

**Interfaces:**
- Produces `pub fn assign_polarities(netlist: &Netlist) -> Result<PolarityAssignment, PolarityError>`.
- Produces `pub fn lower_optimised(netlist: &Netlist) -> Result<Netlist, LowerError>` that calls assignment then assigned lowering.

- [ ] **Step 1: Write failing assignment tests against a two-gate dependency**

```rust
#[test]
fn assignment_prefers_a_producers_negative_output_when_its_only_consumer_reads_that_polarity() {
    let source = netlist(
        &["a", "b", "c"],
        &["y"],
        vec![gate(GateKind::And, "p", &["a", "b"]), gate(GateKind::Nand, "y", &["p", "c"])],
    );
    assert_eq!(assign_polarities(&source).unwrap()[0], SignalPolarity::Negative);
}

#[test]
fn assignment_is_deterministic_for_the_baked_decoder() {
    let first = assign_polarities(&verilog::seven_segment().netlist).unwrap();
    assert_eq!(first, assign_polarities(&verilog::seven_segment().netlist).unwrap());
}
```

- [ ] **Step 2: Run the tests and verify they fail because assignment is absent**

Run: `cargo test --lib assignment_prefers assignment_is_deterministic`

Expected: compilation failure naming `assign_polarities`.


- [ ] **Step 3: Implement deterministic whole-netlist local search**

The graph has reconvergent fan-out, so per-row dynamic programming is wrong: one producer's one cached inverse can serve many consumers, and a local score would charge it more than once. Start all eligible gate-level cells at `Positive`. Score a *complete candidate* by calling `lower_with_assignment`, then counting the actual lowered netlist after `NetlistBuilder` has shared every inverter:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LoweredScore { area: u32, gates: usize, torch_depth: u32 }

fn score(netlist: &Netlist, assignment: &[SignalPolarity]) -> Result<LoweredScore, LowerError> {
    let lowered = lower_with_assignment(netlist, assignment)?;
    Ok(score_realisable_netlist(&lowered))
}
```

Visit eligible gates in stable index order. For each, flip one polarity, score the complete lowering, and retain the strictly lowest lexicographic score. Repeat full passes until none improves. Then test every two-gate flip once, also in stable lexicographic index order, and repeat single-gate descent after accepting a pair. This is an iterative whole-circuit optimiser, not an unsupported claim of mathematical global optimality; it sees sharing and fan-out exactly because every candidate is lowered in full.

`score_realisable_netlist` sums actual `Nor(n)`/`Or(n)` ground footprints for `area`, counts actual gates for `gates`, and computes maximum torch levels over `Netlist::topological_order` for `torch_depth`. The order is intentional: first fewer reserved blocks, then fewer placed cells, then fewer torch delays. Routing cost is not guessed here; final compile measurements decide whether an accepted static win is retained.

Return `PolarityError::CyclicNetlist` if `topological_order()` is `None`, and `PolarityError::OutputHasNoProducer { output }` when a declared output cannot be traced to a gate or input.

- [ ] **Step 4: Add end-to-end truth-table and no-regression measurements**

```rust
#[test]
fn optimised_lowering_preserves_every_verilog_decoder_vector() {
    let gate_level = verilog::seven_segment().netlist;
    let lowered = lower_optimised(&gate_level).unwrap();
    assert_truth_table(&lowered, &SEVEN_SEGMENT_TABLE);
}
```

Run: `cargo test --release --test verilog_frontend -- --nocapture`

Expected: Verilog decoder truth table passes and the output reports no more than 56 lowered gates, 12,348 blocks, and 88 ticks. Record the actual numbers in the assertion/doc comment; only retain the pass if it improves at least one measured quantity without worsening another.

- [ ] **Step 5: Commit the optimiser only with a measured win**

```bash
git add src/compile/polarity.rs src/compile/lowering.rs tests/verilog_frontend.rs
git commit -m "feat(lowering): assign gate polarities globally"
```

If the measured result is not Pareto-better than the 56 / 12,348 / 88 baseline, revert this task's production change and keep only the independently useful topology/lowering capability.

### Task 4: Fan-in packing and real-game confirmation

**Files:**
- Modify: `src/compile/polarity.rs` or `src/compile/topology.rs`, depending on whether packing changes selection or recipe shape
- Modify: `tests/verilog_frontend.rs`
- Modify: `docs/superpowers/specs/2026-08-09-polarity-assignment.md`
- Test: `conformance/circuit_conformance.py` command invocation only

**Interfaces:**
- Keeps `assign_polarities` deterministic.
- Allows selected expansions to use `Nor(3)` and `Or(3)` where a boolean-equivalent fan-in packing lowers measured cost.

- [ ] **Step 1: Write a failing test that shows a three-input pack is selected only when it lowers expansion cost**

```rust
#[test]
fn assignment_can_select_a_three_input_final_merge() {
    let source = three_input_or_shaped_gate_level_netlist();
    let lowered = lower_optimised(&source).unwrap();
    assert!(lowered.gates.iter().any(|gate| gate.kind == GateKind::Or(3)));
}
```

- [ ] **Step 2: Run the test and verify it fails before packing exists**

Run: `cargo test --lib assignment_can_select_a_three_input_final_merge`

Expected: assertion failure because the selected lowering only contains two-input merges.

- [ ] **Step 3: Implement only the proven 2-to-3 fan-in pack**

Select `Nor(3)`/`Or(3)` only where all three operands already occur at the same final recipe stage and the packed step is boolean-equivalent. Do not introduce associative rewriting across arbitrary gate boundaries in this task.

- [ ] **Step 4: Verify all layers and measure**

Run: `./check.sh`; `cargo test --release --test verilog_frontend -- --nocapture`; `cargo run --release --bin mc_dump -- verilog:seven_segment > $env:TEMP\reda-verilog-seven-segment.txt`; then run `conformance/circuit_conformance.py` against a fresh real Minecraft 1.20.1 world.

Expected: all automated checks pass, 16/16 vectors pass in the real game, and the docs state the exact gate/block/tick result and commit.

- [ ] **Step 5: Commit measured result and documentation**

```bash
git add src/compile/topology.rs src/compile/polarity.rs tests/verilog_frontend.rs docs/superpowers/specs/2026-08-09-polarity-assignment.md
git commit -m "feat(topology): pack polarity-optimised fan-in"
```

## Plan Self-Review

- Spec coverage: Tasks 1–3 implement the polarity spec's stated order; Task 4 implements the separately measured fan-in packing. The new 3D planner and sequential logic are explicitly outside this plan.
- No placeholders: each task states exact files, interfaces, failing behaviour, command, and expected result.
- Type consistency: `SignalPolarity` is declared in Task 1, `PolarityAssignment` in Task 2, and `assign_polarities`/`lower_optimised` in Task 3 before Task 4 consumes them.
