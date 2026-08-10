# Directed Dust and Diode Inputs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make REDA model weakly powered diode rear inputs and let the router replace legal terminal repeaters with directed redstone dust, validated in the simulator and a real Minecraft 1.20.1 server.

**Architecture:** Keep ordinary block propagation unchanged: weak power never re-drives dust and comparator side inputs remain strong-only. Add a dedicated diode-rear read path for repeaters and comparators. Then add an explicit route-terminal choice, planned before emission and shared by reservation construction and block placement, so `DirectedDustIntoSupport` is selected only for a legal, live dust endpoint; all other routes retain `RepeaterIntoSupport`.

**Tech Stack:** Rust 2021, existing redstone simulator, `conformance/` Python Source-RCON harness, vanilla Minecraft 1.20.1, existing `./check.sh`.

## Global Constraints

- Minecraft behaviour is a hard constraint: test it with a live 1.20.1 RCON probe before relying on it in routing.
- Keep weak block propagation separate from strong block-to-dust propagation.
- Comparator rear input and repeater rear input accept a non-zero weak or strong block; comparator side input remains strong-only.
- `verify_connectivity`, `verify_torch_merge`, and `verify_signal_strength` stay unconditional constraints.
- Every terminal candidate sees a live `Reservation`, including earlier accepted candidates and their keep-out.
- Do not change gate topology, Boolean lowering, floorplanning, or editor code in this plan.
- `./check.sh` and real-game `verilog:and4` conformance must pass before completion.

---

## File structure

- `src/redstone/simulator/propagate.rs` owns the distinction between ordinary block propagation and diode rear-input reads.
- `src/redstone/simulator/component.rs` owns repeater/comparator input semantics and calls the new rear-input helper.
- `conformance/probes.py` owns the game-ground-truth test for weak block diode inputs.
- `src/compile/mod.rs` owns terminal planning, reservation, emission, and structural/signal invariants.
- `src/compile/mod.rs`'s `#[cfg(test)]` module owns minimal terminal geometry regressions.
- `conformance/circuit_conformance.py` stays generic; only use it to verify the changed circuit unless its parser needs a factual new block state.

### Task 1: Model weakly powered rear blocks for repeaters and comparators

**Files:**
- Modify: `src/redstone/simulator/propagate.rs:297-320`
- Modify: `src/redstone/simulator/component.rs:169-179, 254-264`
- Test: `src/redstone/simulator/component.rs` test module

**Interfaces:**
- Consumes: `block_signal_at(world, pos) -> (BlockPower, u8)`.
- Produces: `pub fn diode_rear_signal(world: &World, rear: Position) -> u8`.
- Consumers: `repeater_input_is_powered` and `comparator_rear_input`.

- [ ] **Step 1: Write failing simulator tests**

Add one test for each diode kind. Each world has a powered straight dust cell,
a conductive block immediately in front of it, and the diode immediately on
the other side of that block. Assert the block is `Weak`, the repeater becomes
powered after `run_until_stable`, and the comparator's rear output is 15.
Add a negative control that weak block power still does not re-drive a dust
cell on another face.

```rust
assert_eq!(block_power_at(&world, rear_block), BlockPower::Weak);
assert!(repeater_input_is_powered(&world, repeater_pos));
assert_eq!(comparator_output(&world, comparator_pos), 15);
assert_eq!(other_face_dust.power, 0);
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test weakly_powered_rear_block -- --nocapture`

Expected: the diode assertions fail because `signal_from` only accepts a
strongly powered rear block.

- [ ] **Step 3: Implement the dedicated rear-input path**

Add `diode_rear_signal`. It first preserves direct dust/component input using
the existing `signal_from` direct paths, then, when the rear position is a
conductive block, returns `block_signal_at(world, rear).1` for either weak or
strong non-zero power. Do not change `signal_from`'s ordinary strong-only
block propagation branch. Replace the two component call sites with this
helper; comparator side input is unchanged.

- [ ] **Step 4: Run focused and simulator tests to verify GREEN**

Run: `cargo test weakly_powered_rear_block -- --nocapture; cargo test redstone::simulator -- --nocapture`

Expected: all focused tests pass; existing weak-block-does-not-redrive-dust
tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/redstone/simulator/propagate.rs src/redstone/simulator/component.rs
git commit -m "fix(simulator): let diodes read weak rear blocks"
```

### Task 2: Pin the rule against a real Minecraft server

**Files:**
- Modify: `conformance/probes.py`
- Test: `conformance/run.py` against `minecraft-server/server/server.properties`

**Interfaces:**
- Consumes: `Slot`, `dust_power`, `slot.check`, and `settle` from the existing
  conformance harness.
- Produces: probe `diode_rear_reads_a_weakly_powered_block`.

- [ ] **Step 1: Write the probe before changing its expected results**

Build two lanes from this exact shape, with the redstone block placed last:

```text
redstone block -> dust -> stone -> repeater/comparator -> output dust
```

Assert input dust power 15, a torch on the stone is off (proves weak power), the
diode has `powered=true`, and its output dust has the expected non-zero power.
Read output dust rather than a lamp: the probe is pinning a diode's rear-input
rule, while a lamp adds its own weak-power and delayed-off semantics. Add a third lane with a
dust cell on another face of the same stone and assert it stays at power 0.

- [ ] **Step 2: Run the single live probe**

Run the 1.20.1 server, then:

```bash
cd conformance
python run.py --properties ../minecraft-server/server/server.properties \
  --only diode_rear_reads_a_weakly_powered_block \
  --out results/1.20.1-diode-rear.json --label 1.20.1-diode-rear
```

Expected: every check is `OK`. Stop the server via RCON after the run.

- [ ] **Step 3: Commit the probe and result**

```bash
git add conformance/probes.py conformance/results/1.20.1-diode-rear.json
git commit -m "test(conformance): pin weak block diode inputs"
```

### Task 3: Plan and emit a directed-dust gate terminal

**Files:**
- Modify: `src/compile/mod.rs:1442-1500` and routing-geometry construction
- Test: `src/compile/mod.rs` `#[cfg(test)]` module

**Interfaces:**
- Consumes: `bent_path_cells`, `bent_path_bends`, `plan_bent_path`,
  `dust_powers_block_toward`, `Reservation`, and `merge_branch_is_bare`.
- Produces: `TerminalKind::{RepeaterIntoSupport, DirectedDustIntoSupport}`
  and one terminal decision for every ordinary gate-input route.
- Consumers: reservation record pass, final `emit`,
  `verify_connectivity`, `verify_torch_merge`, and `verify_signal_strength`.

- [ ] **Step 1: Write failing compile tests**

Create a minimal NOR gate with a route approaching a west/east/south socket.
The positive geometry has the final dust cell adjacent to the conductive
support, has its opposite side connected to the route, has no perpendicular
attachment, and arrives with positive strength. Assert the emitted socket is
`RedstoneWire`, the support is reached by the declared input, and the truth
table is correct.

Add three negative tests: a terminal cell with a perpendicular attachment, a
dead final dust cell, and a bare merge branch. Assert each uses `Repeater` at
the socket and the circuit remains correct.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test directed_dust_terminal -- --nocapture`

Expected: positive test fails because `lay_bent_path` unconditionally changes
the final cell to a repeater.

- [ ] **Step 3: Add an explicit terminal plan**

Create `TerminalKind` and store its decisions in routing geometry before both
the footprint-record pass and real emission. For an ordinary NOR input only,
evaluate the final `bent_path_cells` cell:

1. it is horizontally adjacent to that gate's conductive support;
2. its route predecessor is on the support-opposite side;
3. `dust_powers_block_toward` would power the support after accounting for
   all planned lateral attachments;
4. the strength plan leaves the final dust cell non-zero;
5. the terminal dust plus its two-cell lateral keep-out can be claimed in the
   live reservation.

Accept `DirectedDustIntoSupport` only if all five conditions hold. Otherwise
select `RepeaterIntoSupport`. Exclude merge branches and routes whose terminal
component is needed as a repeater refresh.

- [ ] **Step 4: Make reservation and emission consume the same decision**

Refactor `lay_bent_path` to receive `TerminalKind`. It must continue to place
budget-driven interior repeaters, but only force the final repeater for
`RepeaterIntoSupport`; dust mode leaves that final cell as dust. The record
pass must claim the same final dust and lateral keep-out before later
candidates are evaluated.

- [ ] **Step 5: Run focused and full compiler tests**

Run:

```bash
cargo test directed_dust_terminal -- --nocapture
cargo test --release --test reference_circuits -- --nocapture
cargo test --release --test verilog_frontend -- --nocapture
```

Expected: all truth tables pass, invariants remain enabled, and at least one
`verilog:and4` input socket is dust rather than a repeater.

- [ ] **Step 6: Commit**

```bash
git add src/compile/mod.rs
git commit -m "feat(router): terminate legal gate inputs with directed dust"
```

### Task 4: Verify physical output in Minecraft and the viewer

**Files:**
- Create: `conformance/results/verilog-and4-directed-dust.json`
- Modify: `docs/superpowers/specs/2026-08-10-directed-dust-termination.md`
- Test: `conformance/circuit_conformance.py`, `./check.sh`, browser viewer

**Interfaces:**
- Consumes: `mc_dump verilog:and4`, generic circuit conformance harness, and
  viewer's existing WASM circuit selection.
- Produces: real-game conformance record for the changed Verilog and4.

- [ ] **Step 1: Build and dump the changed circuit**

Run:

```bash
cargo run --release --bin mc_dump -- verilog:and4 > /tmp/verilog-and4.txt
```

Inspect the dump: at least one gate input support has adjacent dust in the
directed orientation; no terminal dust has a perpendicular attachment.

- [ ] **Step 2: Run real-game conformance**

Start the local 1.20.1 server and run:

```bash
python conformance/circuit_conformance.py \
  --dump /tmp/verilog-and4.txt \
  --properties minecraft-server/server/server.properties \
  --out conformance/results/verilog-and4-directed-dust.json \
  --label verilog-and4-directed-dust
```

Expected: all 16 vectors pass, every gate-level reading matches, and the
server stops cleanly through RCON.

- [ ] **Step 3: Run full project verification**

Run: `./check.sh`

Expected: root tests, viewer tests, both clippy invocations, and the WASM
build all pass.

- [ ] **Step 4: Commit evidence and update the spec's measured results**

Record actual repeater, block, and settle deltas in
`docs/superpowers/specs/2026-08-10-directed-dust-termination.md`; do not claim
theoretical upper bounds as measured results.

```bash
git add conformance/results docs/superpowers/specs/2026-08-10-directed-dust-termination.md
git commit -m "test(router): verify directed dust in Minecraft"
```
