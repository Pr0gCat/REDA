# Sequential Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Accept Yosys `$_DFF_P_` cells, realise them as a redstone master-slave storage element, analyse their clock period, and certify a four-bit register in Minecraft Java 26.2.

**Architecture:** Keep the gate-level `Netlist` explicit about sequential boundaries. A new `GateKind::DffPosedge` cuts dependency edges for combinational ordering, while a separate stateful topology entry owns the internal feedback of the physical implementation. The compiler retains all four physical invariants; sequential state changes are driven only through an external clock input. A real 26.2 client performs paste and lever clicks, while RCON reads the resulting state; RCON `/setblock` is not used to claim dynamic diode behaviour.

**Tech Stack:** Rust 2021, Yosys JSON frontend, REDA simulator and compiler, Litematica 26.2 schema 7.1, vanilla Minecraft Java 26.2 with RCON readback, `./check.sh`.

## Global Constraints

- Target is Minecraft Java **26.2**; 1.20.1 results are historical only.
- A purely combinational cycle remains `CompileError::CyclicNetlist`.
- A legal feedback path must contain `GateKind::DffPosedge`; do not merely disable cycle validation.
- `verify_connectivity`, `verify_torch_merge`, `verify_signal_strength`, and spacing remain unconditional legality constraints.
- The clock is an external input port; REDA does not emit a free-running oscillator.
- `compile()` never lowers implicitly; every caller passes one and the same lowered netlist to compilation, timing, dump, viewer, and litematic export.
- Dynamic whole-circuit Minecraft proof must originate in a real 26.2 client action, not RCON forcing block states.

---

## File structure

- `src/compile/topology.rs` defines `GateKind::DffPosedge`, its pin order `(D, C)`, stateful truth contract, and the master/slave primitive topology alternatives.
- `src/compile/mod.rs` exposes a deterministic combinational dependency order that cuts at sequential gates, validates source signals, and eventually places the sequential macro.
- `src/frontend/yosys_json.rs` maps `$_DFF_P_` with pins `D, C` into the gate-level netlist instead of rejecting it.
- `src/compile/lowering.rs` preserves `DffPosedge` while lowering each combinational island; it never attempts to decompose a register into a combinational NOR graph.
- `src/compile/primitive_graph.rs` represents a sequential cell's owned feedback region without using a DAG-only expansion routine for that region.
- `src/timing/mod.rs` adds register-to-register setup-period analysis beside the existing input-to-output settle model.
- `src/circuits/verilog.rs`, `tests/sequential_frontend.rs`, and `tests/sequential_compile.rs` own small register fixtures and simulator-level regressions.
- `conformance/` owns a client-paste manifest and RCON readback record; it does not claim that `/setblock` toggled a clock.

### Task 1: Make sequential boundaries explicit in the gate-level graph

**Files:**
- Modify: `src/compile/topology.rs`
- Modify: `src/compile/mod.rs`
- Test: `src/compile/mod.rs` test module

**Interfaces:**
- Produces `GateKind::DffPosedge` with `arity() == 2`, pin 0 = `D`, pin 1 = `C`, and `is_sequential() -> bool`.
- Produces `Netlist::combinational_order() -> Option<Vec<usize>>`; an edge into a sequential gate does not contribute to the order, and each sequential output is available as a source to downstream combinational gates.
- Existing `Netlist::topological_order()` remains a compatibility alias for `combinational_order()` after all callers are migrated.

- [x] **Step 1: Write failing legality tests**

Build these two same-shape netlists directly with `Gate` literals:

```rust
let through_dff = Netlist {
    inputs: vec!["clk".into()],
    outputs: vec!["q".into()],
    gates: vec![
        gate(GateKind::Nor(1), "d", &["q"]),
        gate(GateKind::DffPosedge, "q", &["d", "clk"]),
    ],
};
assert_eq!(through_dff.combinational_order(), Some(vec![1, 0]));

let pure_loop = Netlist {
    inputs: vec![], outputs: vec!["a".into()],
    gates: vec![gate(GateKind::Nor(1), "a", &["b"]), gate(GateKind::Nor(1), "b", &["a"])],
};
assert_eq!(pure_loop.combinational_order(), None);
```

Also assert `GateKind::DffPosedge.fixed_arity() == Some(2)`, that `wire_name()` is `"dff_p"`, and that a malformed one-input DFF is rejected by the existing arity validation.

- [x] **Step 2: Run the tests and verify RED**

Run: `cargo test sequential_boundary --lib -- --nocapture`

Expected: compilation fails because `DffPosedge`, `is_sequential`, and `combinational_order` do not yet exist.

- [x] **Step 3: Add the minimal graph semantics**

Add the `DffPosedge` enum variant and extend every exhaustive `GateKind` method. In `Netlist::combinational_order`, build producer/dependent edges only when the *consumer* is not sequential; enqueue sequential gates initially alongside primary-source gates, but retain their deterministic source index ordering. This makes a feedback edge ending at DFF a state boundary rather than a zero-delay combinational edge.

- [x] **Step 4: Verify GREEN**

Run: `cargo test sequential_boundary --lib -- --nocapture; cargo test topological_order --lib -- --nocapture`

Expected: the DFF loop has an order; the all-combinational loop stays rejected; all existing ordering tests pass unchanged.

- [x] **Step 5: Commit**

```bash
git add src/compile/topology.rs src/compile/mod.rs
git commit -m "feat(netlist): cut dependency order at DFF boundaries"
```

### Task 2: Preserve DFF cells through the Verilog and lowering boundaries

**Files:**
- Modify: `src/compile/topology.rs`
- Modify: `src/frontend/yosys_json.rs`
- Modify: `src/compile/lowering.rs`
- Test: `src/frontend/yosys_json.rs` test module
- Test: `tests/sequential_frontend.rs`

**Interfaces:**
- `gate_kind_for_yosys_cell("$_DFF_P_") == Some(GateKind::DffPosedge)`.
- `CELL_PINS` maps `$_DFF_P_` to `["D", "C"]` in that order.
- `lower()` copies a DFF gate unchanged while recursively lowering its combinational input cones.

- [ ] **Step 1: Write failing frontend and lowering tests**

Parse a minimal JSON module with one `$_DFF_P_`, a `D` input, a `C` input, and a `Q` output. Assert the netlist has exactly one `DffPosedge` gate whose `inputs == ["d", "clk"]`. Then call `lower()` and assert the DFF remains one gate with the same output, while an `$_AND_` feeding `D` is lowered to realisable gates.

- [ ] **Step 2: Verify RED**

Run: `cargo test dff_frontend -- --nocapture`

Expected: the frontend reports `$_DFF_P_` as unsupported.

- [ ] **Step 3: Implement the boundary mapping**

Add only `$_DFF_P_` to the topology/Yosys tables. Add a `DffPosedge` arm in assigned and ordinary lowering that copies the gate after checking exact arity; no polarity assignment is attempted across this state boundary. Ensure `GateKind::evaluate` is not used for DFF, because its result depends on previous state and a clock transition rather than just its two current boolean pins.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test dff_frontend -- --nocapture; cargo test lowering --lib -- --nocapture`

Expected: the DFF survives, adjacent combinational gates lower, all combinational frontend tests stay green.

- [ ] **Step 5: Commit**

```bash
git add src/compile/topology.rs src/frontend/yosys_json.rs src/compile/lowering.rs tests/sequential_frontend.rs
git commit -m "feat(frontend): preserve positive-edge DFF cells"
```

### Task 3: Add a stateful primitive-graph topology without weakening graph checks

**Files:**
- Modify: `src/compile/primitive_graph.rs`
- Modify: `src/compile/topology.rs`
- Test: `src/compile/primitive_graph.rs` test module

**Interfaces:**
- `PrimitiveGraph` gains a stateful-region record containing the DFF’s `D` landing, clock landing, `Q` contributor, and the primitives internally owned by the cell.
- `expand()` uses `Netlist::combinational_order()` for inter-cell edges and accepts an internal DFF feedback cycle only inside that record.
- `ExpandError::CyclicNetlist` remains the result for a graph whose cycle contains no sequential cell.

- [ ] **Step 1: Write failing graph tests**

Expand the `through_dff` circuit from Task 1 with a library that contains the DFF entry. Assert its graph contains a DFF-owned primitive region and an edge from the DFF `Q` contributor to the `d` NOR. In a separate test, expand `pure_loop` and assert `Err(ExpandError::CyclicNetlist)`.

- [ ] **Step 2: Verify RED**

Run: `cargo test primitive_graph_accepts_dff_feedback --lib -- --nocapture`

Expected: DFF has no library entry or expansion still fails as cyclic.

- [ ] **Step 3: Implement a latch-first DFF entry**

Represent one level-sensitive latch as a cross-coupled pair of `Primitive::Torch` nodes plus named data/enable landing nodes. Define `DffPosedge` as two latch regions, master enabled when `C` is low and slave when `C` is high. Keep the internal feedback edges in the region; do not call the DAG layout routine over them. Expose only `D`, `C`, and `Q` as inter-cell ports.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test primitive_graph_accepts_dff_feedback --lib -- --nocapture; cargo test primitive_graph_rejects_combinational_cycle --lib -- --nocapture`

Expected: a DFF feedback cycle is represented; a bare NOR loop is still rejected.

- [ ] **Step 5: Commit**

```bash
git add src/compile/topology.rs src/compile/primitive_graph.rs
git commit -m "feat(topology): represent DFF feedback as a stateful region"
```

### Task 4: Realise and simulate the latch/DFF macro

**Files:**
- Modify: `src/compile/mod.rs`
- Modify: `src/redstone/simulator/component.rs`
- Modify: `src/redstone/simulator/mod.rs`
- Test: `tests/sequential_compile.rs`

**Interfaces:**
- `compile()` recognises `GateKind::DffPosedge` and places its fixed master-slave latch macro with explicit D/C/Q ports.
- `Simulator` exposes a stable sequential observation after each externally applied clock transition.
- `CompiledCircuit` records DFF output positions so dump/viewer/conformance use a single `Q` source.

- [ ] **Step 1: Write failing functional tests**

Create a one-bit DFF netlist with inputs `d`, `clk` and output `q`. The test sequence is `d=0, clk=0; d=1, clk=0; clk=1; d=0, clk=0; clk=1`. After each `run_until_stable`, assert Q is respectively `0, 0, 1, 1, 0`. Also compile a two-bit independent DFF pair and assert toggling one D input cannot alter the other Q.

- [ ] **Step 2: Verify RED**

Run: `cargo test dff_holds_until_rising_clock --test sequential_compile -- --nocapture`

Expected: `compile()` reports `NotRealisable` for `DffPosedge`.

- [ ] **Step 3: Implement the macro and port routing**

Place two named level-sensitive latch substructures for each DFF, reserve their internal cells before ordinary routing, and emit D/C/Q ports as ordinary nets with typed directions. Run the four existing physical invariants over all external ports and a dedicated stateful-macro structural check over internal feedback, rather than pretending its two cross-coupled torches form a combinational net.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test --test sequential_compile -- --nocapture; cargo test redstone::simulator --lib -- --nocapture`

Expected: the sequence holds state correctly, independent bits remain isolated, and existing combinational truth tables pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/mod.rs src/redstone/simulator/component.rs src/redstone/simulator/mod.rs tests/sequential_compile.rs
git commit -m "feat(compile): place a master-slave redstone DFF"
```

### Task 5: Analyse sequential timing and expose the artifact

**Files:**
- Modify: `src/timing/mod.rs`
- Modify: `src/bin/mc_dump.rs`
- Modify: `src/bin/build_circuit.rs`
- Test: `tests/sequential_compile.rs`

**Interfaces:**
- `TimingSummary` gains `register_to_register_period_game_ticks: Option<u32>`.
- `mc_dump` emits `SEQUENTIAL DFF_P <name> D=<signal> C=<signal> Q=<signal>` and Q observation positions.
- `build_circuit` exports a four-bit register `.litematic` using the same lowered netlist as `mc_dump`.

- [ ] **Step 1: Write failing timing and dump tests**

For a DFF followed by a NOR and a second DFF, assert the period equals the measured combinational torch/repeater path between Q and D, excluding the DFF’s own storage delay. Assert `mc_dump` has one `SEQUENTIAL` record per DFF and exactly four records for the `reg4` fixture.

- [ ] **Step 2: Verify RED**

Run: `cargo test register_to_register_period -- --nocapture`

Expected: no sequential timing field or dump record exists.

- [ ] **Step 3: Implement the analysis and artifact contract**

Cut dependency walks at DFF inputs and start them at DFF Q outputs plus primary inputs. Reuse actual compiled repeater counts exactly as `critical_path_settle_model_game_ticks` does. Emit the sequential lines from the same compiled-netlist metadata; do not parse placement back out of a World.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test register_to_register_period -- --nocapture; cargo test --test verilog_litematic -- --nocapture`

Expected: register period is exact in the simulator and existing Verilog litematic lowering remains unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/timing/mod.rs src/bin/mc_dump.rs src/bin/build_circuit.rs tests/sequential_compile.rs
git commit -m "feat(timing): report register-to-register period"
```

### Task 6: Certify a four-bit register in a real Minecraft 26.2 client

**Files:**
- Create: `conformance/results/reg4-26.2-client.json`
- Modify: `conformance/circuit_conformance.py`
- Modify: `docs/minecraft-server.md`
- Test: `./check.sh`, manual client/RCON run

**Interfaces:**
- Conformance input is an ordered vector sequence, not an unordered truth table.
- A client-paste manifest names the generated `.litematic`, world origin, four D levers, the clock lever, and four Q lamps.
- RCON only reads and records block states after the client clicks the described levers.

- [ ] **Step 1: Write a failing sequence parser test**

Add a Python unit test whose sequence is `D=0101,C=0; D=1010,C=0; C=1; C=0; C=1` and whose expected Q samples are `0000,0000,1010,1010,1010`. Assert the old truth-table-only harness rejects this fixture as unsupported.

- [ ] **Step 2: Verify RED**

Run: `python -m unittest conformance.test_circuit_conformance.SequentialSequenceTest -v`

Expected: failure because the sequence type/parser does not exist.

- [ ] **Step 3: Implement client-assisted readback**

Add a `--sequence` JSON input and `--client-paste` manifest output. The harness waits for each state reported by RCON, but never uses `/setblock` to change D or C. It records every observed lamp state, the client action requested, server version `26.2`, and the litematic SHA-256.

- [ ] **Step 4: Run a real client session**

Start `minecraft-server-26.2`, paste the generated `reg4.litematic` through the installed 26.2 client/Litematica, then physically click the named levers in the recorded sequence. Run RCON readback and save `conformance/results/reg4-26.2-client.json` only if every state matches.

- [ ] **Step 5: Verify and commit**

Run: `./check.sh`

```bash
git add conformance docs/minecraft-server.md
git commit -m "test(conformance): verify reg4 in Minecraft 26.2"
```

## Plan self-review

- **Coverage:** Task 1 preserves combinational-loop rejection; Tasks 2–4 bring Yosys DFF through topology, lowering, physical realisation and simulator state; Task 5 supplies the separate sequential timing metric; Task 6 provides the only valid 26.2 dynamic proof and preserves downloadable litematics.
- **No fake verification:** no task sets a repeater or latch output to powered in order to claim a clock transition; all dynamic proof comes from a client click.
- **Representation boundary:** DFF feedback is internal to one stateful topology region; inter-cell graph ordering stays a DAG after sequential cuts.
- **Compatibility:** direct combinational netlists retain their current lowering and compile path, and every task requires existing tests to remain green.
