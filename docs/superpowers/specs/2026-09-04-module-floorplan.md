# Module Floorplan: hierarchical placement over the topology-aware seed

Status: approved in design review on 2026-09-04 (four sections, each
confirmed by the user); revised the same day after a code-backed review
(§13 lists what changed).

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
macro library. Nothing since has designed it. The front end loses the
hierarchy on the Rust side: `synth.py` never flattens and Yosys keeps every
module in its JSON, but `netlist_from_json` reads only the top module and
reports a submodule instance cell as a missing `Y` connection
(`yosys_json.rs:397`) before it ever checks the cell type.

## 2. Goals

1. A design written as modules compiles as blocks: every distinct module
   is compiled and certified once; every instance is a translated copy.
2. The parent places blocks and its own loose gates with the existing
   level/column placer and wires the gaps with the existing channel plan
   and guarded router. No new router.
3. The certified artifact is flat. Hierarchy exists only during placement
   and routing. Structural verification, the symbolic equivalence proof,
   the exhaustive truth check and the manifest sweep run unchanged on the
   union, and the union contains no block-specific object.
4. A single-module design compiles through the new path to the same
   candidate as today (same candidate fingerprint), so the six acceptance
   cases and the eleven pinned seven-segment IO are unaffected.
5. Block compiles are independent and may run on separate threads. The
   merged result is deterministic and identical to a sequential run.
6. An 8-bit ALU built as two 4-bit ALUs built from slices certifies.

## 3. Non-goals

- No automatic recovery of repeated subgraphs from a flat netlist. Reuse
  comes from module instances the user wrote.
- No pinned ports on blocks. A block is compiled exactly as an unpinned
  circuit is today; its ports are wherever the seed's automatic ports land
  (§6 explains why that is enough and cheaper).
- No compositional certification. Each block is certified when compiled,
  and the assembled design is certified again in full. No per-block timing
  certificate replaces whole-world simulation.
- No integration of `macro_cells::CellLibrary`; no fixed geometry library.
- No change to the A* router, `RouterLimits`, `SynthesisInput`,
  `compile_fragment_synth`, `ExpandedPhysicalCandidate`,
  `PhysicalEndpointId`, the pinned-IO contract or the legacy front doors.
- No fragment optimisation inside a block from the parent: blocks compile
  at budget zero in this milestone (§9).
- No stateful modules. A module containing a flip-flop is refused by the
  existing `UnsupportedStatefulTopology` path.
- No block rotation. Every block keeps the seed's frame: inputs west,
  outputs east, ground at y = 1.

## 4. Chosen architecture

```
Verilog ── Yosys (no flatten) ── HierarchicalNetlist
                                        │
                     ┌──────────────────┴──────────────────┐
                     │ per module, parallel:                │
                     │   compile block = unpinned seed +    │
                     │   certify                            │
                     └──────────────────┬──────────────────┘
                                        │ CompiledBlock {candidate, bounds, ports}
   parent: planning graph with block macros + loose gates
         → placement (blocks are macros) → channel plan/layout → routes
         → union candidate (blocks translated, routes spliced)
         → certify (unchanged)
```

Recursion is uniform: a module that instantiates other modules is itself
compiled by the parent procedure, and the result is a `CompiledBlock` for
its own parent. The top module is the root of the same procedure with the
caller's pins, if any.

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

`synth.py` already runs `hierarchy -check -top`, `proc`, `opt`, `techmap`,
`abc` and `opt_clean` with no selection and no `flatten`, so every pass
runs per module and the JSON carries every module. Only the reader
changes: `netlist_from_json` reads every module; a cell whose `type` is a
key of `modules` becomes a `ModuleInstance`; the type check moves ahead of
the `Y` lookup so any other unknown type is still reported as an unknown
cell, not as a missing connection. Parameterised modules appear under
Yosys's derived names (`$paramod\full_adder\WIDTH=4`) both as module keys
and as instance types; the reader keeps those strings as the module key
and the flattener sanitises them for signal prefixes. Multi-bit ports are
still split per bit with the existing `port[i]` names, in the instance
port map as well.

### 5.3 Constant ports

An instance port tied to a constant (`1'b0` on a slice's carry-in) is not
a different pin contract; it is a different module. The front end
specialises: it clones the module under a derived name
(`full_adder@cin=0`), substitutes the constant into the clone's own gates
(the existing NOR/OR constant folding in `netlist_from_json` applies) and
rewrites the instance to the clone. Two instances with the same constants
share the clone.

### 5.4 Flattening

`HierarchicalNetlist::flatten() -> (Netlist, Vec<GatePath>)` produces the
flat netlist certification uses. Signal names inside an instance are
prefixed with the instance path joined by `.` (`alu.slice2.g7`); port
connections are resolved by aliasing the child port name to the parent
signal, so no extra buffer gates appear. `GatePath` records, for every flat
gate, the instance path it came from. Flattening is deterministic: modules
in `BTreeMap` order, instances and gates in declaration order.

Lowering runs per module, on the module's own gates with its ports as
boundaries, and the flattened netlist that certification uses is the
flattening of the *lowered* modules, never a second lowering of the
flattened gates. `lower_optimised` may otherwise optimise across a module
boundary and make two instances of one module differ, which would break
sharing and the one-to-one correspondence through `GatePath`.

### 5.5 Test builder

`HierarchicalNetlistBuilder` in `src/circuits/` wraps `NetlistBuilder` and
adds `instance(name, module, ports)`. The hierarchical test circuits of
§11 are built with it; one Verilog fixture (`tests/fixtures/ripple_adder8.v`
with a `full_adder` module) exercises the Yosys path end to end.

## 6. Block compile and port table

A block is compiled by the existing unpinned seed, with no
`PortPlacements`, at budget zero, and certified. Nothing about the seed
changes for a block. The result is read back:

```rust
pub struct CompiledBlock {
    pub module: String,
    pub candidate: ExpandedPhysicalCandidate, // as compiled, including the
                                              // seed's own translation to x,z >= 16
    pub bounds: BlockBounds,                  // min/max x, y, z over every claim
    pub inputs: BTreeMap<String, BlockPort>,  // west edge
    pub outputs: BTreeMap<String, BlockPort>, // east edge
    pub metrics: CandidateMetrics,
}

pub struct BlockPort {
    pub cell: Anchor,     // input: the lever cell; output: the lamp cell
    pub toward: Facing,   // always East in this milestone
}
```

Why no pins. Pinning even only the inputs sends the seed down the pinned
path (pin column, router stubs, level folding, frame turning). The
acceptance data shows what that costs: the pinned seven segment has 56% of
the gates of the unpinned one and 2.3 times the ticks and 1.5 times the
blocks. The unpinned seed already puts every automatic input one input
channel west of the first level (a stone under a lever, the route starting
one cell east of the lever) and every automatic output one channel east of
the last level (an output terminal repeater feeding a lamp). Those cells
are a pin table by construction: an input port is the lever cell with
`toward = East`, an output port is the lamp cell with `toward = East`. The
parent reads them from `candidate.boundaries`.

Port order along an edge is decided by the block's own placement (the row
of the macro each port wires to) and is not controllable; the parent's
channel plan takes the rows as they are, as it does for any macro's ports.

## 7. Parent placement and routing

### 7.1 Planning graph

The parent plans over the existing `InstanceGraph` with one addition: a
`blocks` list (`BlockInstance { id, block, path, inputs, output_gates }`)
that is skipped by serialisation when empty, so a flat design's fingerprint
is unchanged. Blocks are **not** `Instance`s: every loop over
`graph.instances` (primitive placement, socket assignment, fragment
proposals) ignores them by construction, and the few places that must see
them (DAG analysis, envelopes, block placement, block routes, channel
occupancy) get explicit block loops.

Block endpoints reuse existing identity variants with the block's own
`InstanceId`, so `PhysicalEndpointId` gains nothing: block output `k` is
`PrimitiveOutput(PrimitiveId { instance: block, node: k })`, block input
`k` is the sink `InstanceInput { instance: block, input_index: k }` whose
landing is `Landing(ConnectionId::External { instance: block, input_index:
k })`. The parent's planning netlist carries one synthetic `Buf` gate per
block output so the signal table can name it; those gates are never
instantiated.

Geometry for a block: an output is a source with anchor at the lamp cell,
`allowed_exit = East`, signal strength 15 (the block's output terminal
repeater feeds it); an input is a sink with anchor at the lever cell,
`allowed_entry = West`, requirement `Exact(OutputTerminalRepeater)` (a
repeater laid on the lever cell facing the block's root dust), support =
the root cell.

Gates that belong to a block are not in the parent graph; they enter the
union candidate at the end (§7.4). The parent's planning candidate is never
certified; only the union is.

### 7.2 Placement

`macro_envelope` for a block returns its bounding box (all four facings
identical; blocks are not rotated). Levels come from the DAG as today; a
block's level is the maximum level of its input drivers plus one. The
delay `analyse_instance_dag` uses for a block is the block's certified
`static_routed_delay`, the worst input-to-output path, so head/tail ticks
and slack are conservative for a block. Lateral tracks, barycentric
sweeps, level folding, channel widths and the column sweep run unchanged;
a block that does not fit the lateral window is `LateralWindowTooNarrow`
naming the block, because a block cannot be folded. `LATERAL_GAP` applies
between a block and its neighbours as between any macros.

### 7.3 Channel plan and layout

A block's ports are channel endpoints exactly like a gate macro's ports:
inputs on a channel's end edge, outputs on the next channel's start edge,
each with its row. Crossing rows are chosen among rows free of every macro
in the column, so a net that must pass a block runs outside the block's
lateral extent. Column escapes, doglegs, jogs and departure planning apply
unchanged.

Before any parent route runs, every block's bounding box, extended one
cell above the block's own maximum y, is reserved as closed cells; the
block's lanes sit at ground + 2 and its closed layers reach ground + 3, so
the air above the whole box is what keeps parent routes out.

### 7.4 Union candidate: translation and splicing

After routing, the parent builds one flat `ExpandedPhysicalCandidate`.

**Translation.** `translate(candidate, dx, dy, dz)` moves every
anchor-carrying field: `placements.{anchor, delayed.at, blocks[].at}`,
`boundaries.{delayed.at, blocks[].at}`, `routes.{cells, floors,
branches[].{root, path[], terminal.at}}`, `junctions.{at, cells}`,
`observations[].site.at`, `pins[].at`, `pin_contracts[].at`. No existing
walker covers all of these (`all_owned_blocks`, `physical_ledger`,
`visit_blocks` and `deterministic_world_size` visit only the four block
arrays), so `translate` is written as a field-by-field walk next to
`fingerprint()`, which is the one function that already touches every
field, and §11 tests it by serialising. `dx, dz` come from the instance's
placed origin minus the block's own origin (the unpinned seed shifts its
plan to x, z ≥ 16, and `bounds` is measured on the shifted claims); `dy` is
the parent's ground y minus 1, which is zero unless the parent is a pinned
top module.

**Splicing.** A block boundary is an ordinary mid-route repeater in the
union. The candidate model allows one route per connection and structural
verification requires a route's source to be the driving endpoint, so the
parent's route and the block's internal route are joined into one
`RealisedRouteTree` rather than meeting at an endpoint:

- *Block input.* The parent's route ends with an exact repeater at the
  lever cell, facing east, on the stone the block placed there. The
  block's internal route from that input (root one cell east of the lever)
  is appended: the parent's branch path continues through the repeater
  into the block route's cells; the block route's branches become branches
  of the parent's tree; the lever, the block's `PrimaryInput` boundary,
  its observation and its pin contract are dropped. The repeater restores
  strength 15, which is what the block's route was compiled against.
- *Block output.* The block's route from its driving gate ends in the
  output terminal repeater facing the lamp. The lamp is removed; the
  parent's route from that output, whose source is now the block's driving
  gate, continues from the lamp cell. The block's `DeclaredOutput`
  boundary and observation are dropped.
- The block's `InstanceId`s, `RouteId`s and `RoutedSinkId`s are renumbered
  into the parent's space in instance-path order; the parent's own
  instances and routes are appended; `refresh_exact_route_delays` is rerun
  on the union so route delays count the boundary repeaters.

A block's input route may itself be a fanout tree, so a spliced tree can
carry a tree; `RealisedRouteTree` allows that. Path continuity and the
"root within one cell of the source" check hold across the splice because
the repeater cell and the root cell are adjacent by construction.

The union is indistinguishable from a flat compile of the flattened
netlist: one gate per instance, absolute anchors, ordinary routes with
repeaters, no block objects.

## 8. Certification

`CompleteCandidateCertifier::certify` runs on the union candidate with the
flattened lowered netlist. Structural verification re-instantiates every
gate's topology and compares; the equivalence proof walks the flattened
combinational order; the exhaustive truth check and the manifest sweep
simulate the emitted world. None of these sees a block, and none of them
changes.

Each block was certified when it was compiled, with its own manifest over
its own inputs. A failure of the assembled design therefore points at the
assembly (a parent route or a splice), which is reported with the instance
path.

## 9. Budgets, fingerprints and determinism

- **Budget.** `compile_hierarchical(netlist, budget, pins)` compiles every
  block at `SynthesisBudget::Evaluations(0)` and spends `budget` on the
  parent's fragment search over its own loose gates and routes: the
  proposal stream keeps choosing fragments from the certified union, and
  every proposal re-plans the parent around the same compiled blocks. A
  proposal that names a block-internal gate is refused, not applied.
  Optimising inside blocks is later work.
- **Case fingerprint.** The parent's `CaseDescriptor` hashes the
  hierarchical netlist (modules, instances, port maps, constant
  specialisations) in addition to today's fields. A single-module design
  hashes the same netlist as today and an empty instance list, and the
  design degenerates to the flat path, so its candidate fingerprint is
  unchanged.
- **Determinism.** Block compiles depend only on the module. They run on a
  thread pool sized by available parallelism and are collected into a
  `BTreeMap<String, CompiledBlock>` keyed by module; every later step
  iterates that map. No wall-clock budget, no randomness, no hash-map
  iteration order reaches the result. A test compiles the same design with
  one thread and with many and compares candidate fingerprints.

## 10. Errors

- `HierarchyError::Cycle { modules }`, `UnknownModule { instance, module }`,
  `PortMismatch { instance, port }` from the front end.
- `BlockCompileError { module, first_path, source: SynthesisError }`: a
  block that does not certify names its module and the first instance path
  that uses it.
- Parent placement and routing errors are today's `SeedError`s, with the
  instance path added where a block is involved; the bounded
  channel-widening repair loop applies to the parent as it does to any
  seed.
- `LateralWindowTooNarrow { block }` when a block is wider than the
  parent's window.

## 11. Tests and acceptance

Unit tests (debug build):

- front end: two-level and three-level JSON with instances parse; a
  `$paramod` instance resolves; a cycle and an unknown module are refused
  as such (not as a missing `Y`); flatten produces prefixed names and a
  `GatePath` per gate; a module with `port[i]` bits maps per bit; a
  constant-tied port produces a specialised module shared by equal
  instances;
- `translate`: serialise the candidate before and after, every `Anchor`
  moved by exactly the offset and nothing else changed; the translated
  candidate passes `validate_shape`;
- block port table: an unpinned compile of `full_adder` yields inputs at
  lever cells on the west edge and outputs at lamp cells on the east edge,
  in `bounds`;
- splicing: a parent with one loose gate driving one block input, and one
  block output driving one loose gate, produces a union whose routes pass
  structural verification and whose route delays include the boundary
  repeaters;
- planning graph: a block macro's envelope equals its bounds; its
  endpoints carry the port anchors and facings;
- a single-module hierarchical netlist yields the same candidate
  fingerprint as `compile_fragment_synth` on the flat netlist, for `and4`,
  `full_adder` and the pinned seven segment (eleven IO byte-identical).

Release-only acceptance (ignored tests, same harness style as
`every_large_circuit`):

| circuit | structure | must |
|---|---|---|
| ripple_adder8 | 8 × full_adder module (slice 0 specialised on cin) | certify |
| alu4_full | 4 × slice module + glue | certify |
| multiplier4 | 3 adder rows as modules, blocks side by side | certify |
| alu8 | 2 × alu4 module, each 4 × slice (three levels) | certify |
| ripple_adder8 via Verilog fixture | Yosys path | certify, same fingerprint as the builder version |

Recorded and reported against the flat compile, not gated: settle ticks,
non-air blocks, wall time, and the share of wall time spent in the
manifest sweep (measured before implementation starts). Every block
boundary adds a repeater tick, so a tick gate against the flat compile
would invite exactly the pass-driven patching this project refuses; the
gate is certification, and alu8 certifying is the milestone.

The six existing acceptance cases are rerun through the harness and must be
byte-identical in candidate fingerprint and metrics.

## 12. Rejected alternatives

- **Slice-aware placement only** (annotate gates with slice index and
  role, keep global routing). Least code, but the wiring problem that
  breaks stays global and generation time barely moves.
- **Pin contracts on blocks** (parent chooses port sides and order, block
  compiled pinned). The first draft of this spec. Dropped: the pinned path
  costs 2.3 times the ticks, the parent cannot control automatic output
  order anyway, and the channel plan already accepts ports on any row.
- **Pins on all four sides with lateral stubs.** Needed only if chained
  blocks were stacked laterally. Level placement makes every inter-block
  net west-to-east, so the feature is unnecessary.
- **Block endpoints as new `PhysicalEndpointId` variants**, or a separate
  planning enum threaded through placement, layout and routing. Both
  drafts. Dropped: the first for its blast radius, the second because every
  planning function is keyed by `InstanceId` already and a block only
  needs one.
- **Compositional certification** (per-block timing certificate, boundary
  simulation only). Forbidden by the fragment-synthesis spec's "no weaker
  verifier" rule and an open research problem; the union candidate keeps
  full certification at no design cost.
- **Automatic detection of repeated subgraphs.** Dropped by the user:
  hierarchy stops at the module, and the module is what the user writes.
- **Integrating `macro_cells`.** It stores geometry without logic; the
  equivalence proof could not see through it.

## 13. Revision after review (2026-09-04)

- §1: the front end does not flatten; only the JSON reader drops the
  hierarchy.
- §6: pin contracts removed; blocks compile unpinned and the port table is
  read from the seed's automatic ports.
- §7.1: blocks are a separate list on the instance graph and their
  endpoints reuse existing identity variants with the block's instance id;
  `PhysicalEndpointId` is unchanged.
- §7.4: boundaries are spliced into one route tree with a repeater at the
  join, because the candidate model has one route per connection; the
  block's signal-strength assumption is stated and met.
- §7.3: keep-out extends one cell above the block's own top, not one cell
  above ground.
- §7.4 and §11: `translate` is a complete field walk tested by
  serialisation, since no existing walker covers every anchor.
- §5.2 and §5.3: `$paramod` names and constant-port specialisation.
- §11: ticks recorded, not gated.
