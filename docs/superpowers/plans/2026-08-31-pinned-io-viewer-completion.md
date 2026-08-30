# Pinned IO Viewer Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the IO-terminal campaign by making the viewer consume the structured pinned-port sidecar, install caller-owned input/output fixtures, preserve baked sessions across reset, and ship the pinned seven-segment glyph artifact.

**Architecture:** Keep the compiler contract unchanged. The baked sidecar is parsed into typed `BakedPort` values; `Session::build_inner` validates each pinned entry against the shipped world, installs caller fixtures into the empty caller cells, and records which inputs need `drive_caller_cell` rather than lever mutation. Every session retains a pristine fixture-installed world for reset. The browser rebuilds 3D geometry when a pinned input changes between Air and RedstoneBlock.

**Tech Stack:** Rust 2021, `serde`, `serde-wasm-bindgen`, REDA `World`/`Simulator`, wasm-bindgen, plain browser JavaScript, Cargo tests.

**Spec:** `docs/superpowers/specs/2026-08-30-io-terminals.md`

## Global Constraints

- A pinned cell belongs to the caller and the compiled `.litematic` must leave it empty.
- Input `toward` means the reader is at `at + toward`; output `toward` means the delivery repeater is at `at - toward`.
- The viewer may install a source or lamp only in the caller-owned pinned cell; it must not alter REDA-owned handover cells.
- Unpinned ports retain their existing lever/lamp behavior and legacy `[x,y,z]` sidecar entries.
- Pinned output labels remain human-facing (`a` through `g`); internal `gN` names do not leak into the sidecar or UI.
- A timeout or malformed sidecar is never reported as physical unsatisfiability.
- The checked-in baked `.litematic` contains no viewer fixtures; fixtures exist only in the viewer session's borrowed world.

---

### Task 0: Close the compiler terminal-isolation gaps

**Files:**
- Modify: `src/compile/planner.rs:3995-4073`
- Modify: `src/compile/planner.rs:6000-6131`
- Test: `src/compile/planner.rs` (`tests` module)

**Interfaces:**
- Consumes: `PortPlacements`, `PinRefusal::NoGapFrom`, `PrimitiveNode::conductors`, and `verify_terminal_contract`.
- Produces: one complete six-face pin-gap rule and a production terminal verifier that sees signal-carrying primitive cells as well as routed cells.

- [ ] **Step 1: Write the failing vertical-adjacency test**

Extend the existing `two_pinned_cells_the_caller_left_no_gap_between_are_refused` coverage with a separate test that pins `a` at `(10,1,10)` and `b` at `(10,2,10)`, both facing north. Assert the complete structured error:

```rust
PlannerError::InvalidPortPin {
    port: "a".to_string(),
    at: Anchor { x: 10, y: 1, z: 10 },
    refusal: PinRefusal::NoGapFrom {
        other_pinned_cell: Anchor { x: 10, y: 2, z: 10 },
    },
}
```

The production mutation this catches is checking only horizontal deltas while the contract reserves all six face-neighbours.

- [ ] **Step 2: Run the vertical test and verify RED**

```powershell
cargo test --lib two_vertically_adjacent_pinned_cells_are_refused -- --exact --nocapture
```

Expected: the test fails because `dx + dz == 0`, so the current `NoGapFrom` branch is skipped.

- [ ] **Step 3: Implement the complete adjacency predicate**

Keep the existing horizontal/one-level dust-climb condition and add the missing same-column face adjacency:

```rust
let horizontally_touching = dx + dz == 1 && dy <= 1;
let vertically_touching = dx == 0 && dz == 0 && dy == 1;
if horizontally_touching || vertically_touching { /* structured refusal */ }
```

- [ ] **Step 4: Run the vertical and existing horizontal tests**

```powershell
cargo test --lib two_vertically_adjacent_pinned_cells_are_refused -- --exact --nocapture
cargo test --lib two_pinned_cells_the_caller_left_no_gap_between_are_refused -- --exact --nocapture
```

Expected: both pass.

- [ ] **Step 5: Write the failing primitive-conductor halo test**

Start from the existing valid pinned-input candidate used by `a_foreign_net_beside_a_pinned_cell_is_refused`. Add a foreign primitive conductor at a non-handover neighbour in the candidate metadata and the corresponding real block in the emitted world, but do not add a route reservation entry. Call `verify_terminal_contract` directly and require `PortTerminalViolation` naming the pin and the adjacent primitive cell.

The production mutation this catches is consulting only `compile::Reservation`, whose `verify_spacing` producer contains route anchors and intentionally omits primitive bodies.

- [ ] **Step 6: Run the primitive-conductor test and verify RED**

```powershell
cargo test --lib a_primitive_conductor_beside_a_pinned_cell_is_refused -- --exact --nocapture
```

Expected: failure because the current invariant sees no route reservation at the intruding cell.

- [ ] **Step 7: Make primitive conductors part of the invariant**

For each of the five non-handover neighbours, check `candidate.primitive_nodes` for a node whose `conductors` contains that cell. Reject it as an adjacent signal-carrying primitive before or alongside the route-owner check. Do not reject inert `Solid`/`Glass` floor or fill cells; the compiler's conductor classification already distinguishes those.

- [ ] **Step 8: Run terminal and planner regressions**

```powershell
cargo test --lib a_primitive_conductor_beside_a_pinned_cell_is_refused -- --exact --nocapture
cargo test --lib a_foreign_net_beside_a_pinned_cell_is_refused -- --exact --nocapture
cargo test --lib the_ports_own_net_beside_its_pinned_cell_is_refused_too -- --exact --nocapture
cargo test --test terminal_handover
```

Expected: all pass.

- [ ] **Step 9: Commit Task 0**

```powershell
git add src/compile/planner.rs
git commit -m "fix(planner): enforce the complete pinned-cell isolation halo"
```

---

### Task 1: Typed baked pinout and caller fixtures

**Files:**
- Modify: `viewer/src/lib.rs:136-151`
- Modify: `viewer/src/lib.rs:770-1018`
- Modify: `viewer/src/lib.rs:1132-1153`
- Test: `viewer/src/lib.rs` (`pinned_baked_session_tests`)

**Interfaces:**
- Consumes: `reda::compile::{drive_caller_cell, input_terminal_reader, output_terminal_handover, probe_caller_cell}` and `planner::{Anchor, PortPin, PortRole}`.
- Produces: `BakedPort`, `BakedPinout`, `InputControl`, and a typed `BakedParts` consumed by `Session::build_inner`.

- [ ] **Step 1: Write a failing real-session test**

Add a `#[cfg(test)] mod pinned_baked_session_tests` in `viewer/src/lib.rs`. Build `and4` with input `a` pinned at `(21,1,62)` toward north and output `y` pinned at `(53,1,10)` toward north through `compile_grown`. Feed the resulting world and structured port metadata into the wished-for typed baked-session constructor. Assert these literal behaviors:

```rust
assert_eq!(session.simulator.world().get(21, 1, 62).kind, BlockKind::Air);
assert_eq!(session.simulator.world().get(53, 1, 10).kind, BlockKind::Lamp);
session.set_lever("a", true).unwrap();
assert_eq!(session.simulator.world().get(21, 1, 62).kind, BlockKind::RedstoneBlock);
```

Turn `b`, `c`, and `d` on, settle, and assert the lamp at `(53,1,10)` is lit. The production mutation this catches is treating a caller cell as if it already contained a lever or lamp.

- [ ] **Step 2: Run the test and verify RED**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml pinned_baked_session_installs_and_drives_caller_fixtures -- --exact --nocapture
```

Expected: compilation failure for the wished-for typed baked pin interface, or behavioral failure because the output cell is Air and setting `a` only toggles `lit` on Air.

- [ ] **Step 3: Add typed sidecar values and validate the handover**

Define:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum BakedPort {
    Unpinned([i32; 3]),
    Pinned {
        at: [i32; 3],
        toward: String,
        handover: [i32; 3],
    },
}

#[derive(Debug, Clone, Deserialize)]
struct BakedPinout {
    inputs: BTreeMap<String, BakedPort>,
    outputs: BTreeMap<String, BakedPort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputControl {
    Lever,
    CallerCell,
}
```

Convert `toward` through one function accepting exactly `north`, `south`, `east`, and `west`. For a pinned entry, construct `PortPin`, require its derived handover to equal the sidecar, and require `input_terminal_reader` or `output_terminal_handover` to find that same coordinate in the shipped world. Return a named `String` error containing the port name and disagreeing coordinates on any mismatch.

- [ ] **Step 4: Install fixtures before creating the simulator**

For pinned inputs, call `drive_caller_cell(&mut world, at, false)` and record `InputControl::CallerCell`. For pinned outputs, call `probe_caller_cell(&mut world, at)`. Keep unpinned coordinates and behavior unchanged. Build `Simulator` only after all fixtures are installed, then settle once.

- [ ] **Step 5: Drive the correct input implementation**

Change `Session::set_lever` to switch on `InputControl`: mutate `BlockState::lit` only for `Lever`; call `drive_caller_cell` for `CallerCell`.

- [ ] **Step 6: Run viewer tests and verify GREEN**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml pinned_baked_session_tests -- --nocapture
cargo test --manifest-path viewer/Cargo.toml
```

Expected: all tests pass.

- [ ] **Step 7: Commit Task 1**

```powershell
git add viewer/src/lib.rs
git commit -m "feat(viewer): exercise pinned ports through caller fixtures"
```

---

### Task 2: Baked reset and mutable geometry

**Files:**
- Modify: `viewer/src/lib.rs:770-805`
- Modify: `viewer/src/lib.rs:1004-1017`
- Modify: `viewer/src/lib.rs:1163-1181`
- Modify: `viewer/src/lib.rs:1234-1241`
- Modify: `viewer/index.html:746-790`
- Test: `viewer/src/lib.rs` (`pinned_baked_session_tests`)

**Interfaces:**
- Consumes: the `InputControl` and typed fixture installation from Task 1.
- Produces: `Session::initial_world: World`, `Pin::pinned: bool`, and reset behavior that never recompiles or loses baked data.

- [ ] **Step 1: Write the failing reset test**

Using the Task 1 pinned `and4` session, turn all four inputs on and settle. Call `reset()`, then assert literal postconditions:

```rust
assert_eq!(session.tick_count(), 0);
assert_eq!(session.simulator.world().get(21, 1, 62).kind, BlockKind::Air);
assert_eq!(session.simulator.world().get(53, 1, 10).kind, BlockKind::Lamp);
assert!(!session.simulator.world().get(53, 1, 10).lit);
```

Turn all inputs on again and require the pinned output lamp to light. The production mutation this catches is `reset()` rebuilding from the circuit name and discarding baked world/pin metadata.

- [ ] **Step 2: Run the reset test and verify RED**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml pinned_baked_reset_restores_the_same_fixture_installed_world -- --exact --nocapture
```

Expected: failure because current `reset()` calls `Session::build(&self.circuit_name)` and therefore does not restore the baked pinned session.

- [ ] **Step 3: Retain the initial world and reset only simulation state**

Add `initial_world: World` to `Session`. After fixture installation and initial settling, store the settled world clone. Implement reset by replacing only `self.simulator` with `Simulator::new(self.initial_world.clone())`, settling it, and preserving topology, port metadata, and the original baked layout.

- [ ] **Step 4: Expose pinned input metadata to the page**

Add `pinned: bool` to serialized `Pin`. Populate it from `InputControl` for inputs and from pinned-output metadata for outputs. Keep `name/x/y/z` unchanged.

- [ ] **Step 5: Rebuild 3D geometry after caller-source topology changes**

In `toggleLever`, find the input pin by name. After settling, call `build3DGeometry()` when `pin.pinned` is true; otherwise keep `update3DStrengths()`. This is required because a pinned input changes between Air and RedstoneBlock, changing both the non-air coordinate set and the index order of `strengths()`.

- [ ] **Step 6: Run reset and full viewer tests**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml pinned_baked_reset_restores_the_same_fixture_installed_world -- --exact --nocapture
cargo test --manifest-path viewer/Cargo.toml
```

Expected: all tests pass.

- [ ] **Step 7: Commit Task 2**

```powershell
git add viewer/src/lib.rs viewer/index.html
git commit -m "fix(viewer): reset baked sessions without losing caller geometry"
```

---

### Task 3: Sidecar compatibility and malformed-data refusal

**Files:**
- Modify: `viewer/Cargo.toml`
- Modify: `viewer/src/lib.rs:1132-1153`
- Test: `viewer/src/lib.rs` (`baked_pinout_format_tests`)

**Interfaces:**
- Consumes: `BakedPort` and `BakedPinout` from Task 1.
- Produces: one deserializer that accepts legacy arrays and structured pinned entries while rejecting invalid facing/handover/world combinations during session construction.

- [ ] **Step 1: Add `serde_json` as a dev dependency and write format tests**

Add `serde_json = "1.0"` under `[dev-dependencies]`. Deserialize these two literal fixtures into `BakedPinout`:

```json
{"inputs":{"a":[1,2,3]},"outputs":{"y":[4,5,6]}}
```

```json
{"inputs":{"a":{"at":[21,1,62],"toward":"north","handover":[21,1,61]}},"outputs":{"y":{"at":[53,1,10],"toward":"north","handover":[53,1,11]}}}
```

Assert exact variants and coordinates. Add a session-construction test whose sidecar reports the wrong handover and assert the error names the port, reported cell, and world/derived cell.

- [ ] **Step 2: Run format tests and verify RED**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml baked_pinout_format_tests -- --nocapture
```

Expected: compilation or deserialization failure before `BakedPort` is implemented completely.

- [ ] **Step 3: Route the wasm wrapper through the typed parser**

Make `Session::from_baked` deserialize `JsValue` directly into `BakedPinout` and pass it to the same typed `build_inner` path exercised by native tests. Do not maintain a separate browser-only interpretation.

- [ ] **Step 4: Run all viewer tests**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml
```

Expected: legacy baked `segment_a` compatibility and new pinned entries both pass.

- [ ] **Step 5: Commit Task 3**

```powershell
git add viewer/Cargo.toml viewer/Cargo.lock viewer/src/lib.rs
git commit -m "fix(viewer): read structured pinned-port sidecars"
```

---

### Task 4: Ship the pinned glyph artifact

**Files:**
- Create: `viewer/baked/verilog_seven_segment.grown.pins.json`
- Replace: `viewer/baked/verilog_seven_segment.grown.litematic`
- Replace: `viewer/baked/verilog_seven_segment.grown.pinout.json`
- Modify: `viewer/README.md`
- Test: `viewer/tests/verilog_circuits.rs`

**Interfaces:**
- Consumes: the shipping pin coordinates in `src/compile/planner.rs::pinned_glyph_decoder` and `build_circuit --grown --pins`.
- Produces: the exact checked-in files fetched by `GROWN['grown:verilog:seven_segment']`.

- [ ] **Step 1: Add the literal shipping pin file**

Create `viewer/baked/verilog_seven_segment.grown.pins.json` with outputs:

```text
a (76,1,24) north; b (84,1,32) east; c (84,1,48) east;
d (76,1,56) south; e (68,1,48) west; f (68,1,32) west;
g (76,1,40) west
```

and inputs `d0..d3` at `(76 + 12*i,1,120)` toward north.

- [ ] **Step 2: Write a fast baked-artifact contract test**

In `viewer/tests/verilog_circuits.rs`, load the checked-in pinout JSON as text through `serde_json`, assert all eleven entries are structured pinned entries, assert `a..g` are the output keys, and load the litematic to require all eleven `at` cells are Air with the reported handover cells recognized by REDA's input/output terminal predicates. This test must not regenerate the circuit.

- [ ] **Step 3: Run the artifact test and verify RED**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml checked_in_grown_decoder_is_the_pinned_glyph -- --exact --nocapture
```

Expected: failure because the current sidecar contains bare arrays and the current litematic is the older unpinned grown decoder.

- [ ] **Step 4: Regenerate through the public CLI**

From the repository root, run:

```powershell
cargo run --release --bin build_circuit -- verilog:seven_segment --grown --pins viewer/baked/verilog_seven_segment.grown.pins.json
```

Copy only these generated files into `viewer/baked/`:

```text
output/verilog_seven_segment.grown.litematic
output/verilog_seven_segment.grown.pinout.json
```

Do not commit the `.blocks.txt` diagnostic output.

- [ ] **Step 5: Run artifact and truth tests**

Run:

```powershell
cargo test --manifest-path viewer/Cargo.toml checked_in_grown_decoder_is_the_pinned_glyph -- --exact --nocapture
cargo test --manifest-path viewer/Cargo.toml the_verilog_seven_segment_session_matches_its_truth_table_through_the_wasm_api -- --exact --nocapture
```

Expected: both pass; the checked-in world is the pinned glyph and the normal viewer decoder remains correct.

- [ ] **Step 6: Update viewer documentation and commit**

Document that `grown:verilog:seven_segment` installs sources/lamps in caller cells at runtime and that its checked-in litematic intentionally contains neither. Then run:

```powershell
git add viewer/baked/verilog_seven_segment.grown.pins.json viewer/baked/verilog_seven_segment.grown.litematic viewer/baked/verilog_seven_segment.grown.pinout.json viewer/README.md viewer/tests/verilog_circuits.rs
git commit -m "feat(viewer): ship the decoder as a pinned digit glyph"
```

---

### Task 5: Integration and live acceptance

**Files:**
- Modify only if verification exposes a defect in files already owned by Tasks 1-4.

**Interfaces:**
- Consumes: all previous tasks.
- Produces: evidence that the campaign acceptance is complete.

- [ ] **Step 1: Run focused native regressions**

```powershell
cargo test --test build_circuit_pins
cargo test --test terminal_handover
cargo test --manifest-path viewer/Cargo.toml
```

Expected: all pass.

- [ ] **Step 2: Run the repository suite**

```powershell
cargo test
```

Expected: all non-ignored tests pass.

- [ ] **Step 3: Build wasm**

```powershell
wasm-pack build viewer --target web
```

Expected: successful `viewer/pkg` build with no Rust compilation errors.

- [ ] **Step 4: Exercise the browser as the caller**

Serve `viewer/`, load `grown:verilog:seven_segment`, and verify:

```text
initial 0000 -> segments a b c d e f on, g off
0010 (digit 2) -> segments a b d e g on, c f off
Reset -> returns to 0000 without recompiling or changing the glyph coordinates
```

In 3D, turn one pinned input on and off and require the caller source to appear/disappear without corrupting other block colours or coordinates.

- [ ] **Step 5: Review the complete branch diff**

```powershell
git status --short
git diff --check
git diff main...HEAD --stat
git log --oneline main..HEAD
```

Expected: clean worktree, no whitespace errors, and only scoped IO-terminal/viewer work.

- [ ] **Step 6: Reconcile the spec with the shipped contract**

Update `docs/superpowers/specs/2026-08-30-io-terminals.md` after all acceptance checks pass:

```text
Status: IMPLEMENTED
"other five neighbours" consistently, rather than "other three"
`toward` is horizontal only
unreachable handover is a named routing-time refusal, not a pre-planning refusal
current source symbol references instead of stale line numbers
```

Commit the documentation separately:

```powershell
git add docs/superpowers/specs/2026-08-30-io-terminals.md
git commit -m "docs(specs): mark the IO-terminal contract implemented"
```
