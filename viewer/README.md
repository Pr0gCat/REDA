# reda viewer

A browser page that runs `reda`'s actual redstone simulator, compiled to
WebAssembly, and draws what it sees. See
`docs/superpowers/plans/2026-08-07-simulating-viewer.md` for the design.

This is two pieces:

- `src/lib.rs` (crate `reda-viewer`): thin `wasm-bindgen` wrappers around
  `reda::compile::compile` and `reda::redstone::simulator::Simulator`. No
  redstone logic lives here -- it only exposes the simulator's own state.
- `index.html`: a single, dependency-free page that loads the compiled wasm
  module and drives it. No build step, no CDN, no npm.

## Rebuilding the wasm module

From `viewer/`, with `wasm-pack` installed and cargo on `PATH`:

```sh
wasm-pack build --target web
```

This regenerates `viewer/pkg/` (`reda_viewer.js`, `reda_viewer_bg.wasm`, and
the `.d.ts` files), which `index.html` imports directly as
`./pkg/reda_viewer.js`. `pkg/` is generated output -- rebuild it after any
change to `src/lib.rs`; it is not meant to be hand-edited.

Run the crate's own tests (drives `and4`'s full truth table through the wasm
API, plus a `slice`-vs-`World::get` consistency check) with:

```sh
cargo test
```

## Running the page

Browsers refuse to load ES modules and `fetch()` the `.wasm` file over
`file://`, so the page needs to be served over HTTP. Any static file server
serving the `viewer/` directory works, e.g. from `viewer/`:

```sh
python -m http.server 8000
```

then open `http://localhost:8000/`.

A ready-made launch entry for this is checked in at `.claude/launch.json`
(configuration name `viewer`, port 8000) for tooling that reads that file.

## Using the page

Three views share one loaded circuit, switched from the header: **2D Slice**,
**3D View**, and **Topology**. The circuit dropdown lists the four hand-written
reference circuits plus the Verilog-derived ones, which the wasm build loads
from the checked-in netlists in `src/circuits/baked/` -- a browser cannot run
Yosys, so those are baked at build time and held to fresh synthesis by
`the_baked_netlists_match_fresh_synthesis`.

### 2D Slice

1. Pick a circuit from the **Circuit** dropdown (populated from
   `list_circuits()`).
2. Pick an **Axis** (X/Y/Z) and drag the **Index** slider to slice through the
   world along it. **Zoom** controls pixels per cell; at 14px and above, each
   cell's signal strength is drawn as a number, and grid lines appear from 6px
   up. The grid is drawn to a canvas, not one DOM node per cell, so this stays
   responsive even for `seven_segment`'s 232x298 = ~69k-cell slices. Screen
   axes follow the same convention as any Minecraft map/schematic tool: a Y
   slice is a top-down view (X horizontal, Z vertical); an X or Z slice is a
   side elevation (the other horizontal axis horizontal, Y vertical, sky at
   the top). The caption above the canvas always names which world axis is
   currently horizontal and which is vertical.
3. Toggle **Inputs** on/off -- each toggle calls `set_lever` and then
   immediately `run_until_stable`, so the effect is visible without an extra
   click. **Outputs** shows the live value at each output pin (read via a
   `slice(Axis.Y, y)` at the pin's coordinates, independent of whatever axis
   the canvas currently displays).
4. **Step** advances exactly one game tick; **Run to stable** runs until
   nothing is left to schedule (or throws if the circuit diverges); **Reset**
   rebuilds the circuit from scratch, discarding lever state and tick count.
5. Hovering the canvas shows the coordinate, block kind, and signal strength
   under the cursor.

### Topology

The Topology tab answers "what is this circuit", which the other two views do
not: they show where the blocks are. It draws `Session::topology()`, which
carries no positions at all, so this view invents its own layout rather than
projecting the world's.

**What is drawn is the primitive graph, and only that** -- the torches,
repeaters, junctions, levers and lamps that really get built. Every edge runs
from the real producer to the real consumer and terminates on it. A gate-level
cell (a `nand`, a `mux`) is drawn as a labelled **hull**: an outline around the
primitives that *are* it. A hull carries no signal, nothing enters or leaves it,
and edges cross its outline without noticing. It is an identification device --
"these four torches are the `nand`" -- not a boundary.

- **Level** picks how that one graph is laid out, not what it is. *Grouped* (the
  default) pulls each cell's members together and hulls them; *Flat* is the same
  nodes and edges with no grouping pressure, so switching between the two shows
  exactly what grouping cost in layout quality.
- **Focus** isolates one cell plus everything one hop from it.
- Solid lines are dust. The only patterned line in the tab is a wire merge's
  dotted spokes, so "not solid means not a connection" is the whole key.
- A shared inverter gets a violet ring: `lower` builds `!d1` once and credits it
  to whichever cell got there first, and the ring is what says the other readers
  are borrowing rather than owning.
- The hand-written circuits are not dressed up. `NetlistBuilder` emits NOR gates
  and merges and nothing else, so each of their cells is a hull around the one
  torch it already was, and the status line says so.

**Three earlier pictures were wrong, each for its own reason, and it is worth
knowing why before proposing a fourth.** Force-directed spaghetti was unreadable
at decoder scale. Drawing the lowered graph alone answered "what did this lower
to", which the 2D and 3D views already answer in more detail -- `verilog:and4`, a
circuit that is three ANDs, read as nine NOR badges. A Level toggle between the
gate level and the primitive level then showed the three ANDs *or* the nine NORs
and never the relationship, leaving the reader to join two pictures from memory.
The last wrong one is the instructive one: drawing a cell as a *container*, with
signal landing on numbered pins at its edge, makes the box a routing boundary,
and nothing in redstone is one -- a torch powers a specific other torch's
support, and `g1` is a name somebody gave to four torches, not a place a signal
goes. The hull is what is left when that implication is removed.

The long-form version of all of this, including what a hull must not be allowed
to imply and how the level-of-detail thresholds were chosen, is the block
comment at "Topology view" in `index.html`. `window.reda` exposes `hulls()` and
`landings()` so both claims the picture makes can be diffed against
`Session::topology()` from a console.

`docs/` holds screenshots of the 2D and 3D views. It held three of the Topology
tab too, taken at `e2ee43e`; they showed the second of the four pictures above
and were deleted rather than kept, because a screenshot of a view that no longer
looks like that is read as documentation of the view that does.

## What's untested from the Rust side

`pinout()` and `legend()` return `JsValue`, which cannot be constructed in a
native `cargo test` (see the module doc comment in `src/lib.rs`). This page is
the first thing that actually exercises them; both were confirmed, by loading
the page in a browser, to come back as plain JS values -- `legend()` as
`[{id, name, colour}]` and `pinout()` as `{inputs: [...], outputs: [...]}`,
each `{name, x, y, z}` -- matching the shapes documented in `src/lib.rs` and
the simulating-viewer plan.
