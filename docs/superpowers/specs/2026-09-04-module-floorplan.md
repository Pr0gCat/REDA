# Module Floorplan: hierarchical placement over the topology-aware seed

Status: approved in design review on 2026-09-04 (four sections, each
confirmed by the user).

This spec adds one level of structure above the topology-aware seed: a
Verilog module becomes a block that is compiled once, certified once, and
stamped wherever it is instantiated. The parent design places blocks as
opaque macros and wires the gaps between them with the existing channel
plan. The certified artifact stays flat, so the verifier, the equivalence
proof, the simulator and the pinned-IO contract are untouched.

## 1. Problem

The seed handles a module of up to roughly fifty gates reliably. Above that
every new circuit exposes a new geometric corner case: the four large test
circuits of 2026-09-03 (`ripple_adder8`, `alu4`, `alu4_full`,
`multiplier4`) needed five new layout rules between them, and `alu4_full`
alone needed three. Generation time grows with the same curve: 105 to 582
seconds per circuit at budget zero.

The user's diagnosis is the one chips use: units are kept apart and joined
along their edges; inside a unit the logic is not separated. A flat netlist
of 250 gates is one large placement and wiring problem. The same design as
four slice instances plus glue is four small problems the seed already
solves, plus a handful of wires between them.

The founding architecture document (`2026-08-05-redstone-eda-design.md`,
§7.2 and §10.1) already asked for this: datapath slices laid out
programmatically, module hierarchy reused for placement, and no black-box
macro library. Nothing since has designed it. The front end flattens
everything: `synth.py` reads only the top module and `netlist_from_json`
rejects any cell that is not a simple gate.

## 2. Goals

1. A design written as modules compiles as blocks: every distinct
   (module, pin contract) pair is compiled and certified once; every
   instance is a translated copy.
2. The parent places blocks and its own loose gates with the existing
   level/column placer and wires the gaps with the existing channel plan
   and guarded router. No new router.
3. The certified artifact is flat. Hierarchy exists only during placement
   and routing. Structural verification, the symbolic equivalence proof,
   the exhaustive truth check and the manifest sweep run unchanged on the
   union.
4. A single-module design compiles through the new path to the same
   candidate as today (same candidate fingerprint), so the six acceptance
   cases and the eleven pinned seven-segment IO are unaffected.
5. Block compiles are independent and may run on separate threads. The
   merged result is deterministic and identical to a sequential run.
6. An 8-bit ALU built as two 4-bit ALUs built from slices certifies.

## 3. Non-goals

- No automatic recovery of repeated subgraphs from a flat netlist. Reuse
  comes from module instances the user wrote.
- No pins on a block's lateral sides. Blocks expose inputs on their west
  edge and outputs on their east edge only (§6 explains why that is
  enough).
- No compositional certification. Each block is certified when compiled,
  and the assembled design is certified again in full. No per-block timing
  certificate replaces whole-world simulation.
- No integration of `macro_cells::CellLibrary`; no fixed geometry library.
- No change to the A* router, `RouterLimits`, `SynthesisInput`,
  `compile_fragment_synth`, the pinned-IO contract or the legacy front
  doors.
- No fragment optimisation inside a block from the parent: blocks compile
  at budget zero in this milestone (§9).
- No stateful modules. A module containing a flip-flop is refused by the
  existing `UnsupportedStatefulTopology` path.

## 4. Chosen architecture

```
Verilog ── Yosys (no flatten) ── HierarchicalNetlist
                                        │
                     ┌──────────────────┴──────────────────┐
                     │ per (module, pin contract), parallel │
                     │   compile block = seed + certify     │
                     └──────────────────┬──────────────────┘
                                        │ CompiledBlock {candidate, bounds, pins}
   parent: InstanceGraph with BlockMacro instances + loose gates
         → placement (blocks are macros) → channel plan/layout → routes
         → union candidate (blocks translated) → certify (unchanged)
```

Recursion is uniform: a module that instantiates other modules is itself
compiled by the parent procedure, and the result is a `CompiledBlock` for
its own parent. The top module is the root of the same procedure with the
caller's pins.

## 5. Front end

### 5.1 Hierarchical netlist

```rust
pub struct HierarchicalNetlist {
    pub top: String,
    pub modules: BTreeMap<String, Module>,
}

pub struct Module {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub gates: Vec<Gate>,               // this module's own gates
    pub instances: Vec<ModuleInstance>, // child blocks
}

pub struct ModuleInstance {
    pub name: String,        // instance name inside the parent
    pub module: String,      // key into `modules`
    pub ports: BTreeMap<String, String>, // child port -> parent signal
}
```

`Module` reuses `Gate` and `GateKind` unchanged. The module graph must be
acyclic; a cycle is a front-end error naming the modules.

### 5.2 Yosys

`synth.py` keeps `hierarchy -check -top` and never runs `flatten`.
`opt`, `techmap`, `abc` and `opt_clean` are run per module (Yosys does
this already when no `flatten` is present). `netlist_from_json` reads every
module in the JSON. A cell whose type is another module in the file becomes
a `ModuleInstance`; a cell of any other unknown type stays a hard error.
Multi-bit ports are still split per bit with the existing `port[i]` names,
in the instance port map as well.

### 5.3 Flattening

`HierarchicalNetlist::flatten() -> (Netlist, Vec<GatePath>)` produces the
flat netlist certification uses. Signal names inside an instance are
prefixed with the instance path joined by `.` (`alu.slice2.g7`); port
connections are resolved by aliasing the child port name to the parent
signal, so no extra buffer gates appear. `GatePath` records, for every flat
gate, the instance path it came from; the parent uses it to group gates
into blocks (§7.1). Flattening is deterministic: modules in `BTreeMap`
order, instances and gates in declaration order.

Lowering runs per module, on the module's own gates with its ports as
boundaries, and the flattened netlist that certification uses is the
flattening of the *lowered* modules, never a second lowering of the
flattened gates. `lower_optimised` may otherwise optimise across a module
boundary and make two instances of one module differ, which would break
sharing and the one-to-one correspondence through `GatePath`.

### 5.4 Test builder

`HierarchicalNetlistBuilder` in `src/circuits/` wraps `NetlistBuilder` and
adds `instance(name, module, ports)`. The hierarchical test circuits of
§11 are built with it; one Verilog fixture (`tests/fixtures/ripple_adder8.v`
with a `full_adder` module) exercises the Yosys path end to end.

## 6. Pin contract

A block's ports are pinned by its parent before the block is compiled:

```rust
pub struct PinContract {
    pub inputs: Vec<String>,   // west edge, north to south, row grid 4
    pub outputs: Vec<String>,  // east edge, north to south, row grid 4
}
```

The contract is relative: side and ordinal only. The parent orders a
block's inputs and outputs by the lateral track order of the nets they
connect to (`track_laterals`, which depends on net intervals and levels
only, not on block sizes), so wires do not cross in the gap. Two
instances of one module with the same ordering share one compile; a
different ordering, or a port tied to a constant, is a different contract
and a different compile.

The block compile turns the contract into `PortPlacements` for the existing
pinned-IO path: inputs at forward `0`, outputs at the block's east edge,
rows at `ROW_GRID` pitch from the north. Because output positions depend
on the block's width, which the compile determines, the seed is asked with
inputs pinned and outputs automatic; the automatic outputs already land on
the east edge at the sweep cursor in row order. The compile reports the
resulting absolute pin anchors and the bounding box:

```rust
pub struct CompiledBlock {
    pub module: String,
    pub contract: PinContract,
    pub candidate: ExpandedPhysicalCandidate, // block-local coordinates, origin 0
    pub bounds: BlockBounds,                  // min/max x, y, z of every claim
    pub inputs: BTreeMap<String, PortPin>,    // west edge
    pub outputs: BTreeMap<String, PortPin>,   // east edge
    pub metrics: CandidateMetrics,
}
```

Why west and east are enough: the parent places by topological level and
every inter-block net leaves a lower level and enters a higher one, so it
always exits an east edge and enters a west edge. Chained blocks (a ripple
carry) occupy consecutive levels and the carry crosses one channel. Blocks
at the same depth sit side by side in one level and never talk to each
other directly.

## 7. Parent placement and routing

### 7.1 Block macros in the instance graph

`InstanceGraph` gains a second kind of instance:

```rust
pub enum InstanceBody {
    Gate(ExpandedInstance),          // today's one-gate instance
    Block(BlockMacro),
}

pub struct BlockMacro {
    pub block: BlockId,              // index into the parent's compiled blocks
    pub path: Vec<String>,           // instance path
    pub inputs: Vec<(PortId, PortIndex)>,   // parent signal -> block input
    pub outputs: Vec<(PortId, PortIndex)>,  // block output -> parent signal
}
```

A block has several outputs, so `PhysicalEndpointId` gains
`BlockOutput(InstanceId, PortIndex)` and sinks gain `BlockInput(InstanceId,
PortIndex)`. Everything that pattern-matches on endpoints (placement
geometry, channel endpoints, route requests, timing arcs) handles the new
arms; the ones that only need an anchor and an exit facing read them from
the block's pin table.

Gates that belong to a block instance are not in the parent graph as
gates; they enter the union candidate at the end (§7.4).

### 7.2 Placement

`macro_envelope` for a block returns its bounding box in every facing (a
block is never rotated in this milestone, so all four are the same box).
Levels come from the DAG as today; a block's level is the maximum level of
its input drivers plus one. `topology_delay_ticks` for a block is the
block's certified `static_routed_delay`, so head/tail ticks and slack stay
meaningful. Lateral tracks, barycentric sweeps, level folding, channel
widths and the column sweep run unchanged. `LATERAL_GAP` applies between a
block and its neighbours as between any macros.

### 7.3 Channel plan and layout

A block's pins are channel endpoints exactly like a gate macro's ports:
inputs on the channel's end edge, outputs on the next channel's start
edge, each with its row. Crossing rows are chosen among rows free of every
macro in the column, so a net that must pass a block runs outside the
block's lateral extent. Column escapes, doglegs, jogs and departure
planning apply unchanged.

The block's whole bounding box plus the air cell above every block is
reserved as closed cells before any parent route runs, so no parent route
enters a block.

### 7.4 Union candidate

After routing, the parent builds one flat `ExpandedPhysicalCandidate`:

- every compiled block's candidate is translated by the instance's placed
  origin (`translate(candidate, dx, dy, dz)`, a new pure function over
  every anchor-carrying field: placements, boundaries, connections, routes,
  junctions, observations, pins);
- the block's `InstanceId`s and `RouteId`s are renumbered into the parent's
  space in instance-path order;
- the block's pinned inputs and automatic outputs become ordinary
  `Landing` sinks and `PrimitiveOutput` sources joined to the parent's
  routes at the handover cells, exactly as a pinned circuit's IO join its
  caller today;
- the parent's own instances and routes are appended.

The union is indistinguishable from a flat compile of the flattened
netlist: one gate per instance, absolute anchors, no block objects. That
is why §8 needs no new code.

## 8. Certification

`CompleteCandidateCertifier::certify` runs on the union candidate with the
flattened lowered netlist. Structural verification re-instantiates every
gate's topology and compares; the equivalence proof walks the flattened
combinational order; the exhaustive truth check and the manifest sweep
simulate the emitted world. None of these sees a block.

Each block was certified when it was compiled, with its own manifest over
its own inputs. A failure of the assembled design therefore points at the
assembly (a parent route or a handover), which is reported with the
instance path.

## 9. Budgets, fingerprints and determinism

- **Budget.** `compile_hierarchical(netlist, budget, pins)` compiles every
  block at `SynthesisBudget::Evaluations(0)` and spends `budget` on the
  parent's fragment search over its own loose gates and routes. Block
  instances have no alternative implementations and are never proposed.
  Optimising inside blocks is later work.
- **Case fingerprint.** The parent's `CaseDescriptor` hashes the
  hierarchical netlist (modules, instances, port maps) and the pin
  contracts, in addition to today's fields. A single-module design hashes
  the same netlist as today and the empty instance list, and the design
  degenerates to the flat path, so its candidate fingerprint is unchanged.
- **Determinism.** Block compiles depend only on (module, contract). They
  run on a thread pool sized by available parallelism and are collected
  into a `BTreeMap<(module, contract), CompiledBlock>`; every later step
  iterates that map. No wall-clock budget, no randomness, no hash-map
  iteration order reaches the result. A test compiles the same design with
  one thread and with many and compares candidate fingerprints.

## 10. Errors

- `HierarchyError::Cycle { modules }`, `UnknownModule { instance, module }`,
  `PortMismatch { instance, port }` from the front end.
- `BlockCompileError { path, module, source: SynthesisError }`: a block
  that does not certify names its module and the first instance path that
  needed that contract.
- Parent placement and routing errors are today's `SeedError`s; the
  bounded channel-widening repair loop applies to the parent as it does
  to any seed.
- A block wider than the parent's lateral window is
  `LateralWindowTooNarrow` naming the block, not a silent fold (a block
  cannot be folded).

## 11. Tests and acceptance

Unit tests (debug build):

- front end: two-level and three-level JSON with instances parse; a cycle
  and an unknown module are refused; flatten produces prefixed names and
  a `GatePath` per gate; a module with `port[i]` bits maps per bit;
- `translate` moves every anchor-carrying field of a candidate and nothing
  else (round trip back to origin is identity);
- `PinContract` to `PortPlacements`: rows on the grid, order preserved;
  two instances with the same ordering share a compile, a different
  ordering does not;
- a block macro's envelope equals its bounds; its endpoints carry the pin
  anchors and facings;
- a single-module hierarchical netlist yields the same candidate
  fingerprint as `compile_fragment_synth` on the flat netlist, for `and4`,
  `full_adder` and the pinned seven segment (eleven IO byte-identical).

Release-only acceptance (ignored tests, same harness style as
`every_large_circuit`):

| circuit | structure | must |
|---|---|---|
| ripple_adder8 | 8 × full_adder module | certify; ticks ≤ flat 474 |
| alu4_full | 4 × slice module (two contracts) + glue | certify; ticks ≤ flat 588 |
| multiplier4 | 3 adder rows as modules, blocks side by side | certify; ticks ≤ flat 591 |
| alu8 | 2 × alu4 module, each 4 × slice (three levels) | certify |
| ripple_adder8 via Verilog fixture | Yosys path | certify, same fingerprint as the builder version |

Also recorded, not gated: non-air blocks and wall time flat vs.
hierarchical, and the share of wall time spent in the manifest sweep
(measured before implementation starts, so the time claim is honest).

The six existing acceptance cases are rerun through the harness and must be
byte-identical in candidate fingerprint and metrics.

## 12. Rejected alternatives

- **Slice-aware placement only** (annotate gates with slice index and
  role, keep global routing). Least code, but the wiring problem that
  breaks stays global and generation time barely moves.
- **Pins on all four sides with lateral stubs.** Needed only if chained
  blocks were stacked laterally. Level placement makes every inter-block
  net west-to-east, so the feature is unnecessary.
- **Compositional certification** (per-block timing certificate, boundary
  simulation only). Forbidden by the fragment-synthesis spec's "no weaker
  verifier" rule and an open research problem; the union candidate keeps
  full certification at no design cost.
- **Automatic detection of repeated subgraphs.** Dropped by the user:
  hierarchy stops at the module, and the module is what the user writes.
- **Integrating `macro_cells`.** It stores geometry without logic; the
  equivalence proof could not see through it.
