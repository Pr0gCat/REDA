# A viewer that simulates

## What this is

A browser page that runs **our own simulator** — the same code the 193 tests
run against, compiled to WebAssembly — and draws what it sees. Not a second
implementation, and not a trace file: if the page shows a signal, that is the
simulator's actual state, and any disagreement between the page and a test is a
bug in one place, not two.

Third-party litematic viewers already draw blocks well. What none of them can do
is show *why* a circuit is wrong: which dust carries what strength, which torch
is about to flip, what changed on this tick. That is what this is for.

## Why it is 2D

Circuits are 232 x **5** x 298 today. A perspective view of a near-planar object
mostly renders occlusion. A flat grid of one slice, with the signal strength
printed in the cell, is both easier to build and better at the job.

"3D" here means slicing along any of the three axes, not perspective rendering.

## The contract

The Rust side is a separate crate under `viewer/` that depends on `reda`. `reda`
itself gains nothing and changes nothing — if this plan requires editing
`src/`, stop and report why.

**No filesystem.** `std::fs` compiles for `wasm32-unknown-unknown` and then
fails at runtime. Circuits come from `reda::circuits`, which are pure Rust
generators needing no files at all. Nothing in the viewer may call
`litematic::load` or `save`.

```
list_circuits() -> string[]

Session::new(circuit_name) -> Session
  size()          -> [x, y, z]
  pinout()        -> { inputs: [{name, x, y, z}], outputs: [{name, x, y, z}] }
  legend()        -> [{id, name, colour}]        block kind id -> display
  set_lever(name, on)
  step()          -> bool     one game tick; false if nothing changed
  run_until_stable() -> u32   ticks taken; errors surface as an exception
  tick_count()    -> u32
  reset()
  slice(axis, index) -> Uint8Array
```

`slice` returns two bytes per cell in row-major order over the slice's two
remaining axes: `[block_kind_id, signal_strength]`. Strength is 0-15 for dust
and 0 or 15 for anything else that is powered — the page does not need to know
redstone rules to colour it.

Block kind ids and their display names come from `legend()`, so adding a block
kind in Rust never requires touching the page.

## The page

One HTML file, no build step, no CDN — it loads the wasm module and nothing
else.

- Circuit picker, populated from `list_circuits()`
- Axis selector (X / Y / Z) and a slider for the index along it
- Canvas grid, one square per cell, coloured by block kind; dust tinted by
  strength, with the number drawn in when zoomed in far enough to read
- A lever per input from `pinout()`, and a readout per output
- Step / run-to-stable / reset, with the tick count visible
- Hovering a cell shows its coordinate, block, and strength

## Order

1. The wasm crate and its API, with a headless Rust test asserting the API
   produces the right answers for `and4` — the same truth table
   `tests/reference_circuits.rs` checks, driven through `set_lever` and
   `run_until_stable` instead of directly. This has to hold before any pixel is
   drawn; otherwise a wrong page and a wrong binding look identical.
2. The page.

## Success

Open the page, pick `and4`, flip all four levers on, watch the lamp light. Flip
one off, watch it go out. Slice to Y=3 and see the routing tracks with live
signal strengths on them.

## Out of scope

- Editing anything. This is a viewer; the floorplan editor is a later phase and
  its controls depend on what the placer ends up exposing.
- Perspective 3D.
- Loading arbitrary `.litematic` files. Circuits come from `reda::circuits`.
  File loading needs the format code, which needs a plan for the wasm/fs split.
