# Task 5 report: lower per module, flatten the lowered modules

## Files

- Modified `src/compile/hierarchy.rs` (added `LoweredHierarchy`, `LowerHierarchyError`,
  `lower_hierarchy`, `impl LoweredHierarchy { block_netlist, as_hierarchical }`, and five
  tests).
- Modified `src/compile/mod.rs` (re-exported `lower_hierarchy`, `LowerHierarchyError`,
  `LoweredHierarchy` alongside the existing `hierarchy::*` re-exports).

## What was built

Exactly the interfaces the brief specifies:

```rust
pub struct LoweredHierarchy {
    pub modules: BTreeMap<String, Module>,
    pub top: String,
    pub flat: Netlist,
    pub paths: Vec<GatePath>,
}
pub enum LowerHierarchyError { Hierarchy(HierarchyError), Lowering { module: String, source: LowerError } }
pub fn lower_hierarchy(design: &HierarchicalNetlist) -> Result<LoweredHierarchy, LowerHierarchyError>
impl LoweredHierarchy {
    pub fn block_netlist(&self, module: &str) -> Netlist;
    pub fn as_hierarchical(&self) -> HierarchicalNetlist;
}
```

`lower_hierarchy`: for every module in `design.module_order()` (children before
parents), lowers `design.boundary_netlist(name)` with `lower_optimised`, and stores a new
`Module` with the original `inputs`/`outputs`/`instances` untouched and `gates` replaced
by the lowered gates. It then builds a fresh `HierarchicalNetlist` from those lowered
modules and calls `.flatten()` on it, filling in `flat`/`paths`. `LowerHierarchyError`
uses `#[from] HierarchyError` for the `module_order`/`flatten` propagation and a named
`Lowering { module, source }` variant that records which module's `lower_optimised` call
failed.

`block_netlist` returns the lowered module's *real* ports only (`module.inputs`/
`module.outputs`), not `boundary_netlist`'s pseudo-ports — the netlist a block compile
takes. `as_hierarchical` just repackages `modules`/`top` back into a
`HierarchicalNetlist`, used both internally (to call `flatten()`) and as a convenience
for callers.

`lower_hierarchy` does **not** call `specialise_constants` itself — same precondition
`flatten()` already documents (a `PortBinding::Zero`/`One` reaching `flatten` is refused
as `UnspecialisedConstant`), and `boundary_netlist` already drops a constant-tied port
from its pseudo-port lists (it only adds `PortBinding::Signal` bindings), so an
unspecialised design surfaces the same error it always did, just one layer up.

## Strict RED/GREEN

The four tests (the brief's two, plus `a_pass_through_module_gets_a_buffer` and the
composition test) were added first, referencing the not-yet-existing `lower_hierarchy`.
`cargo test --lib compile::hierarchy` failed with four `E0425: cannot find function
lower_hierarchy` compile errors — confirmed RED. The implementation above was then added
and the same command passed clean — confirmed GREEN. No implementation code was written
before the RED run.

## The two things the task brief flagged as unverified — both checked directly against source

**1. Does `lower_optimised` preserve a boundary netlist's declared input/output names?**
Yes, verified by reading `lower_with_assignment_and_provenance` and
`lower_with_provenance` in `src/compile/lowering.rs`: both return
`Netlist { inputs: netlist.inputs.clone(), outputs: netlist.outputs.clone(), gates }` —
the port name lists are cloned verbatim, never rewritten. Internally, only intermediate
expansion steps get generated names (`fresh_prefix` — `n0`, `n1`, ...); the *final* step
of any expansion always carries the source gate's own `name`/`output` via
`nor_named`/`merge_named(&gate.name, &gate.output, ...)`, which is exactly what makes a
declared output's name survive. This is also exercised directly:
`lowering_per_module_then_flattening_equals_flattening_the_lowered_single_module` and
`two_instances_of_one_module_lower_identically` both compare gates whose `inputs`/
`output` fields are the literal port names `a`/`y`/`mid`/`x`/`z`, and pass.

**2. Does a pass-through module (an output that is also an input) break `lower_optimised`,
requiring the two-inverter buffer the Yosys reader synthesizes?** No — traced and then
confirmed by a passing test, `a_pass_through_module_gets_a_buffer`. This scenario is real
and does arise naturally: `boundary_netlist` on a module with no gates of its own that
routes one of its own declared inputs straight into a child instance (e.g. `two_level()`'s
`top`, or the single-signal design the test constructs directly:
`Module { inputs: ["a"], outputs: ["a"], gates: [] }`) produces a boundary `Netlist` where
a name appears in both `inputs` and `outputs` with zero gates. Two things make this
harmless in the current code, both read directly from `src/compile/polarity.rs` and
`src/compile/lowering.rs`:

- `polarity::validate_outputs` accepts a declared output whose name matches a declared
  *input* (`is_primary_input`), without requiring a gate producer — it only rejects an
  output that is neither a primary input nor any gate's output.
- With zero gates, the polarity assignment is trivially all-positive, which routes
  through the "compatibility" path (`lower_with_provenance`); with no gates to walk, it
  returns the netlist unchanged (`gates: []`), and the input/output name lists (already
  literally identical for the pass-through signal) are copied through as-is.

So the identity is preserved by construction, not by an added buffer: `for_true`/
`for_false` assignments through `eval::evaluate` on `lowered.flat` both round-trip
correctly in the test. I kept the brief's requested test name
(`a_pass_through_module_gets_a_buffer`) but its own doc comment says plainly that no
buffer gate is added or needed — flagging this loudly rather than silently adding dead
buffering code the brief's own worry turned out not to require. I also checked the
genuinely gate-producing form of a pass-through (`GateKind::Buf`, what the Yosys frontend
actually emits for `assign y = a;`) separately by reading `topology::expansion_for`:
`Buf` already has a real two-step expansion (`NOT` then `NOT`, i.e. exactly the reader's
"two chained inverters"), so a module whose pass-through is represented as an explicit
`Buf` gate (rather than an empty boundary) was already fully handled before this task —
nothing to add there either.

## Composition test

`lowering_a_hierarchical_design_is_exactly_the_union_of_lowered_instance_gates_and_preserves_the_function`,
built on `circuits::ripple_adder(2)` (from `src/circuits/hierarchical_builder.rs`, test-only):

- Groups `lowered.flat.gates` by `lowered.paths[i].path` (the exact instance path), and
  for every group asserts its gate-**kind** sequence, in order, equals
  `lowered.modules[<that path's module>].gates`'s own kind sequence exactly — i.e. every
  flattened gate is traceable to a whole, unmodified copy of some already-lowered
  module's gate list, never a gate a cross-module optimisation could only have produced.
  Also asserts the per-path totals sum to exactly `lowered.flat.gates.len()` (nothing
  extra, nothing missing).
- Asserts every gate in `lowered.flat` is `Nor`/`Or` only.
- Exhaustively enumerates all `2^5` assignments of `ripple_adder(2)`'s five inputs
  (`a0, a1, b0, b1, cin`) and checks `eval::evaluate` (from
  `hierarchical_builder::eval`, `#[cfg(test)]`) agrees between the unlowered
  `design.flatten()` result and `lowered.flat` on every declared output — proving the
  per-module lowering pass didn't just keep the right gate count/shape but also the right
  function. (`ripple_adder`'s hand-built gates are already `Nor`/`Or`-only via
  `NetlistBuilder`, so `eval::evaluate`, which only understands those two kinds, can score
  both netlists.)

## Verification

- `cargo test --lib compile::hierarchy` — 17 passed, 0 failed (13 pre-existing + 4 new).
- `cargo test --lib compile::lowering` — 19 passed, 0 failed (untouched; confirms no
  regression in the module this task consumes).
- `cargo check --lib --tests` — compiles clean; the only warnings present are
  pre-existing dead-code warnings in unrelated files (`hierarchical_builder.rs`'s
  test-only helpers, `fragment_synth/placement.rs`, `yosys_json.rs::netlist_from_json`),
  confirmed unrelated by `git diff --stat` (only `hierarchy.rs`/`mod.rs` touched).
- No other test suites were run per the task instructions.

## Global constraints check

- `SynthesisInput`, `compile_fragment_synth`, `ExpandedPhysicalCandidate`,
  `PhysicalEndpointId`, and the legacy front doors: untouched.
- `lower_optimised`'s behaviour on a flat netlist: untouched — `lower_hierarchy` only
  calls it per-module on `boundary_netlist`, never on a flattened design, and no line
  inside `lowering.rs`/`polarity.rs` was edited.
- Determinism: `BTreeMap` used for `LoweredHierarchy::modules` (matching `Module`'s own
  storage); no `HashMap`/`HashSet` introduced in `hierarchy.rs`.
- Rust 2021, no new dependencies (`thiserror`, already a project dependency, is what
  `HierarchyError` already used).

## Concerns

None outstanding. The one thing worth a reviewer's eye: `lower_hierarchy` propagates
`HierarchyError` from both `module_order()` and `flatten()` through the same
`LowerHierarchyError::Hierarchy` variant via `#[from]`, so a caller cannot tell from the
error variant alone which of the two calls failed (only `HierarchyError`'s own variant
distinguishes, e.g. `Cycle` can only come from `module_order`). This matches the brief's
literal enum shape (`Hierarchy(HierarchyError)`, no extra field), so I did not add one.

## Fix round 1

Review finding: every fixture this task's tests use (`inv`, `full_adder`, `ripple_adder`)
is built only from `GateKind::Nor`/`Or`. `assign_polarities`'s `eligible` list
(`src/compile/polarity.rs`, ~line 81-86) only ever contains non-realisable gates, so with
an all-Nor/Or netlist the assignment is trivially all-`Positive` and
`lower_with_assignment_and_provenance` (`src/compile/lowering.rs`) always takes the
"compatibility" branch (~line 219-224). The mixed-polarity branch, and the positive-rail
correction inside it (the `nor_named` re-materialisation guarded by
`polarity == Negative && netlist.outputs.contains(...)`, ~line 352-363) — the *only* thing
that keeps a per-module-lowered child's declared output from handing its parent an
inverted signal, given `lower_hierarchy` lowers each module in isolation — was never
exercised. The property was true but untested.

**Gate kinds used, and why.** `GateKind::And` and `GateKind::Nand`, chained: `m = a AND b`
(a declared module output), `y = m NAND c` (also a declared module output). This specific
shape was chosen, not just "any non-realisable kind", because a lone non-realisable gate
whose output is directly a declared boundary output is *never* assigned `Negative` by the
optimiser: computing it negative needs the same input rails as computing it positive, plus
the correction's extra gate, so it is strictly dominated (worse `gates` count, which the
score tuple `(area, gates, torch_depth)` compares before `torch_depth`). Forcing `Negative`
onto a declared output's own producer requires a second, internal consumer of that same
signal that benefits from reading its negative rail directly — mirroring the existing
`assignment_prefers_a_producers_negative_output_when_its_only_consumer_reads_that_polarity`
test in `polarity.rs`, except here the producer (`m`) is *also* a declared output, so the
correction has to fire on top of the sharing benefit. `Nand`'s positive expansion consumes
`negative(pin0)`/`negative(pin1)` (`positive_expansion_for(GateKind::Nand)` in
`topology.rs`), so `y` wants `!m`. Hand-verified cost trade for `m`'s own polarity, holding
`y` positive: realising `m` positive costs `Nor(negative(a), negative(b))` (1 gate, named
`m` directly) plus a fresh inverter for `y`'s `!m` (1 gate) = 2 gates around `m`+`y`'s
shared boundary, using `Nor(2)` (area 9, `nor_footprint_area(2)`). Realising `m` negative
costs `Merge(negative(a), negative(b))` (1 gate, generic name) plus the positive-rail
correction (1 gate, named `m`) = 2 gates too, but the `Nor(2)` is replaced by a `Merge(2)`
(area 6, `merge_footprint_area(2)`) and `y` reuses `m`'s already-computed negative rail for
free instead of needing its own inverter — same total gate count, strictly lower area. Area
is the first field the score tuple compares, so the local search always prefers it.

**Evidence the mixed-polarity branch, and specifically the correction, are actually taken.**
Not inferred — confirmed directly. The new test calls `compile::polarity::assign_polarities`
on the leaf module's own boundary netlist (the exact netlist `lower_hierarchy` lowers it on)
before asserting anything else, and asserts the result is
`[SignalPolarity::Negative, SignalPolarity::Positive]` — `m`'s producing gate (index 0) is
negative, `y`'s (index 1) stays positive. Since `m` is a declared output and its gate is
assigned `Negative`, `lower_with_assignment_and_provenance`'s correction condition
(`polarity == Negative && netlist.outputs.iter().any(|o| o == &gate.output)`) is provably
true for this run — that is exactly the branch under test. This was verified empirically,
not just hand-derived: `cargo test --lib compile::hierarchy` was run after writing the
fixture and the `assign_polarities` assertion passed on the first attempt, matching the
hand-computed area trade above.

**What the test asserts** (`a_declared_output_lowered_onto_its_negative_rail_still_reaches_the_parent_positive`,
`src/compile/hierarchy.rs`):
1. `assign_polarities(&boundary)` on the leaf's own boundary netlist returns
   `[Negative, Positive]` — confirms by construction that the fixture takes the targeted
   branch, before anything else is asserted.
2. `lowered.modules["leaf"].gates.len() > leaf.gates.len()` — the leaf really was expanded
   (a non-realisable gate always grows under expansion), and every lowered gate is
   `Nor`/`Or` only.
3. Two instances (`u0`, `u1`) of the leaf, matched through `GatePath`, lower to the
   identical `GateKind` sequence as `lowered.modules["leaf"].gates` itself — the same
   property `two_instances_of_one_module_lower_identically` checks, now under a real
   (not all-positive) polarity assignment.
4. The child's declared output really is on the positive rail, proved by evaluation, not
   by reading gate kinds: the unlowered flattening (`design.flatten()`, which still
   contains the original `And`/`Nand` gates) is evaluated with a new test-local
   `evaluate_any_kind` helper — built from the production `GateKind::evaluate` (handles
   every kind) plus `Netlist::combinational_order()`, since
   `hierarchical_builder::eval::evaluate` only understands `Nor`/`Or` and would panic on
   this fixture's `And`/`Nand` gates — and compared against `eval::evaluate(&lowered.flat, ...)`
   over all `2^6` assignments of the six top-level inputs. Had the positive-rail correction
   been missing, `m0`/`m1` would come out inverted and this comparison would fail on every
   mask where `a & b` is true.

**Commands run:**
- `cargo test --lib compile::hierarchy` — 18 passed, 0 failed (17 pre-existing + 1 new).
- `cargo test --lib compile::lowering` — 19 passed, 0 failed (untouched; no regression).
- `cargo check --lib --tests` — compiles clean; only the same pre-existing dead-code
  warnings noted in the original report (unrelated files, confirmed by `git diff --stat`
  touching only `src/compile/hierarchy.rs`).

No production code was touched: `lower_hierarchy`, `lower_optimised`, `assign_polarities`,
and everything in `lowering.rs`/`polarity.rs`/`topology.rs` are unmodified. This is a
test-only addition to `src/compile/hierarchy.rs`'s `#[cfg(test)] mod tests`.
