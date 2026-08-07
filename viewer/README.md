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

1. Pick a circuit from the **Circuit** dropdown (populated from
   `list_circuits()`).
2. Pick an **Axis** (X/Y/Z) and drag the **Index** slider to slice through the
   world along it. **Zoom** controls pixels per cell; at 14px and above, each
   cell's signal strength is drawn as a number, and grid lines appear from 6px
   up. The grid is drawn to a canvas, not one DOM node per cell, so this stays
   responsive even for `seven_segment`'s 232x298 = ~69k-cell slices.
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

## What's untested from the Rust side

`pinout()` and `legend()` return `JsValue`, which cannot be constructed in a
native `cargo test` (see the module doc comment in `src/lib.rs`). This page is
the first thing that actually exercises them; both were confirmed, by loading
the page in a browser, to come back as plain JS values -- `legend()` as
`[{id, name, colour}]` and `pinout()` as `{inputs: [...], outputs: [...]}`,
each `{name, x, y, z}` -- matching the shapes documented in `src/lib.rs` and
the simulating-viewer plan.
