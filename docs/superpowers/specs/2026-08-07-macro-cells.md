# Macro cells and the floorplan input

## Why this exists

A seven-segment display has no logic in it. It is seven independent lamps whose
*positions relative to each other* are the entire point — put them in a row and
the circuit still works, but nobody can read a digit off it.

Nothing in an HDL can express that. Verilog and VHDL describe which signal
drives which, and deliberately say nothing about coordinates. This is not a
limitation we should route around; real EDA has the same split and solves it
with separate inputs:

| Input | Carries |
|---|---|
| Verilog / VHDL | logic: what drives what |
| LEF | a cell's outline and pin locations |
| GDSII | the cell's actual geometry |
| DEF | placement: which instance sits where |
| SDC | timing constraints |

A standard cell's layout is never *written*. Somebody draws it, saves it, and
the tools consume an abstract of it that has an outline and pins and nothing
else.

REDA already has the file format that plays GDSII's role: `.litematic`. A macro
cell is a `.litematic` plus a manifest naming its pins. That is the whole idea.

## The two inputs to `compile()`

Today `compile(&Netlist)` takes one argument. It gains a second:

```rust
compile(&Netlist, &Floorplan) -> Result<CompiledCircuit, CompileError>
```

`Netlist` stays purely logical — no coordinates, ever. Every spatial decision
that is not the placer's to make lives in `Floorplan`. This mirrors Verilog vs
DEF, and it is the reason a netlist produced by Yosys can be fed in unmodified.

`Floorplan::default()` must reproduce today's behaviour exactly, so every
existing caller and test keeps working by passing it.

## Cell format

```
cells/<cell_name>/
  cell.litematic    the geometry
  cell.toml         the pin manifest
```

```toml
name = "seven_segment_display"
size = [9, 3, 11]

[[pins]]
name      = "a"
at        = [4, 0, 10]
side      = "south"
direction = "input"
```

`size` is asserted against the `.litematic`'s own size rather than derived from
it. A manifest that disagrees with its geometry is an error, not something to
silently reconcile — the two files are edited by different means (one in
Minecraft, one in a text editor) and will drift.

`at` is cell-local, origin at the `.litematic`'s corner. `side` names which face
of the cell the wire approaches from; it must be consistent with `at` lying on
that face's boundary plane.

### Validation, at load time

A cell that fails any of these is rejected with a diagnostic naming the pin:

- `at` is inside the cell's bounds
- `at` lies on the boundary plane named by `side`
- the block at `at` is one the router can legally connect to
- pin names are unique within the cell
- no two pins share a coordinate
- `size` matches the `.litematic`

The point of validating at load is that a broken cell fails when the library is
read, not 400 lines into a placement run.

### What the manifest is *for*

The mapping from `a` to "the top horizontal bar" exists **only** in the
manifest. Whoever builds the display puts pin `a` next to the top bar. The
compiler sees a 9×3×11 opaque box with seven connection points on its south
face; it does not know what a figure-eight is and never needs to.

This is what makes the cell replaceable. A better display — hand-built, or
lifted from the redstone community — drops in by overwriting two files, with no
code change.

## Placement: periphery only, for now

A macro sitting in the middle of the routed area would force the router to
avoid obstacles. It currently assumes it owns every cell it can address, and
teaching it otherwise is a real piece of work.

IO cells do not need it. Real chips put them in a ring around the outside for
the same reason. So the first version constrains macros to the **outside** of
the layout's bounding box, attached to a named edge:

```toml
# floorplan.toml
[[instance]]
cell = "seven_segment_display"
edge = "output"          # the -Z edge, where output pins already emerge
offset = 0               # along the edge, from the layout's centre

[instance.connect]
a = "seg_a"              # cell pin -> netlist signal
b = "seg_b"
```

The bounding box grows to include the macro. Inside the routing region there is
still not a single obstacle, so the router is unchanged apart from where a route
may terminate.

Obstacle-aware routing is deferred until somebody wants a hand-optimised adder
in the core. When that lands, `edge` becomes one option among several rather
than the only one.

## Interaction with the existing output lamps

`compile()` currently drives every netlist output with its own redstone lamp.
That behaviour stays for outputs no macro claims. An output named in some
instance's `connect` table routes to that pin **instead of** getting a lamp —
two drivers on one signal is a bug, not a feature.

`output_positions` reports the lamp for unclaimed outputs and the pin coordinate
for claimed ones, both in world coordinates, so the existing truth-table tests
keep reading whatever is actually the readable endpoint.

## Where the shipped display comes from

The seven-segment display is generated by a small Rust routine and **written out
as `cells/seven_segment_display/cell.litematic`**, not special-cased inside the
compiler.

This costs nothing and buys the thing that matters: the pipeline consumes a file
from day one. There is no privileged built-in path for the compiler to keep
working through while the file-based one rots. Replacing the display is
overwriting a file.

## Out of scope

- Obstacle-aware routing (macros in the core)
- Pins labelled by in-schematic signs — that needs block-entity support, which
  is already queued for comparator output signals
- Rotating or mirroring a cell at placement time
- Cells containing logic that the netlist must reason about; these are geometry
  only
