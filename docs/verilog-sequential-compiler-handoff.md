# Verilog Sequential Compiler Handoff

## Goal

Support Verilog containing `always @(posedge clk)` / DFFs through the complete REDA pipeline:

```text
Verilog
→ Yosys `$_DFF_P_`
→ REDA `GateKind::DffPosedge`
→ Design H primitive graph
→ physical placement and routing
→ redstone world
→ simulator and viewer
```

## Scope clarification: sequential logic versus a clock generator

The first target should be sequential logic driven by an external `clk` input:

```verilog
always @(posedge clk)
    q <= d;
```

This is different from a self-running redstone clock. Standard synthesizable Verilog does not support delay-based clock generators such as:

```verilog
always #5 clk = ~clk;
```

A DFF also still requires a clock source. If REDA must eventually produce an autonomous oscillator, add an explicit clock-source primitive or a verified baked oscillator. Do not make arbitrary combinational feedback legal: such circuits depend on physical propagation delays and would violate the compiler's existing DAG assumptions.

## Existing foundations

### 1. The frontend recognizes a positive-edge DFF

`src/frontend/yosys_json.rs` recognizes:

```text
$_DFF_P_
D, C → Q
```

and converts it to:

```rust
GateKind::DffPosedge
inputs = [d, clk]
output = q
```

There is currently a test using hand-written Yosys JSON. There is no end-to-end test that sends a real `always @(posedge clk)` module through Yosys.

### 2. The netlist distinguishes legal sequential feedback

`Netlist::combinational_order()` in `src/compile/mod.rs` treats a DFF as a dependency boundary.

This feedback is legal:

```text
Q → combinational logic → DFF.D → Q
```

Pure combinational feedback must remain illegal:

```text
NOR A → NOR B → NOR A
```

Do not solve DFF support by allowing every cycle.

### 3. Lowering preserves the DFF boundary

`src/compile/lowering.rs` seeds every DFF Q rail as a source for the current combinational cycle, lowers the feedback cone, and connects the resulting D signal back to the DFF.

An existing test confirms that a feedback cone such as:

```text
q → AND → d
         ↓
       DFF(q)
```

survives lowering.

### 4. Design H topology is defined

`src/compile/topology.rs` defines the fixed Design H positive-edge DFF:

```text
D ───────▶ M_DATA ───────▶ S_DATA ───────▶ Q
C ───────▶ M_LOCK ──side-lock──▶ M_DATA
C ───────▶ INV_C ────────▶ S_LOCK ──side-lock──▶ S_DATA
```

It contains:

- `M_DATA`: repeater
- `S_DATA`: repeater
- `M_LOCK`: repeater
- `S_LOCK`: repeater
- `INV_C`: redstone torch

The complete topology and phase contract are documented in `docs/superpowers/specs/2026-08-11-dff-design-h.md`.

### 5. The primitive graph expands DFFs

`src/compile/primitive_graph.rs` already:

- creates all five Design H primitives;
- connects D to `M_DATA`;
- connects clock to both `M_LOCK` and `INV_C`;
- defines Q as the output of `S_DATA`;
- stores ordinary signal edges separately from repeater side-lock relations;
- resolves feedback across the DFF boundary.

The logical topology layer is therefore mostly complete.

### 6. The simulator supports the required physical behavior

The simulator already implements:

- repeater delay;
- repeater side locking;
- torch inversion;
- scheduled ticks;
- exact game-tick stepping through `Simulator::step()`.

Repeater locking is implemented in `src/redstone/simulator/component.rs`.

The main missing work is in the physical compiler, not the simulator.

## Current blockers

### 1. Compile validation rejects DFFs

`checked_topological_order()` in `src/compile/mod.rs` currently requires every gate to satisfy:

```rust
gate.kind.is_realisable()
```

`DffPosedge` is not a normal NOR/OR realisable gate, so compilation returns `CompileError::NotRealisable` before placement.

Validation should accept either:

1. an ordinary realisable combinational gate with valid arity; or
2. a registered supported sequential gate with valid arity.

A DFF must still have exactly two inputs in `D, C` order.

Removing this validation alone is insufficient because later physical stages currently mishandle the DFF.

### 2. Relaxation assumes one body per gate

`src/compile/relax/build.rs` takes only the first entry from:

```rust
graph.gate_nodes[gate_index]
```

for every non-merge gate.

A Design H DFF has five primitive nodes. If validation is merely relaxed, only `M_DATA` will be placed and the other four primitives will disappear.

The DFF must create all five bodies and place them as a fixed or explicitly constrained macro.

### 3. Side-lock placement constraints are not connected

`Weld::BesideAt` already exists in `src/compile/relax/build.rs`, but currently has no caller.

It should encode:

```text
M_LOCK beside M_DATA
S_LOCK beside S_DATA
```

The lock repeater must face the left or right side port of the data repeater. A side-lock relation must never be routed as an ordinary rear input.

### 4. Candidate construction hard-codes non-merge gates as torches

`candidate_from_anchors_and_facings()` in `src/compile/planner.rs` currently produces only:

```rust
NodeRealisation::WireMerge
NodeRealisation::Primitive(Primitive::Torch)
```

Consequently, a DFF reaching this stage would be emitted as a torch.

The candidate must preserve the five primitive-graph nodes, or introduce an explicit stateful macro realisation. The safest first version is a fixed Design H macro that may rotate as a unit but whose internals may not be freely rearranged.

### 5. `gate_footprint()` only emits NOR or merge cells

`gate_footprint()` in `src/compile/mod.rs` currently calls either:

```rust
place_merge_gate(...)
place_nor_gate(...)
```

A DFF needs a dedicated footprint or macro emitter containing:

- four correctly oriented repeaters;
- support blocks below the repeaters;
- one torch and its support block;
- internal dust;
- D rear landing;
- C rear landing;
- Q front source;
- two side-lock landings;
- a keep-out region preventing external dust from coupling to macro internals.

### 6. The router does not yet consume typed side-lock relations

`src/compile/physical.rs` already defines:

```rust
PortKind::RepeaterRear
PortKind::RepeaterSide
PortKind::RepeaterFront
```

and each repeater variant preserves stable left/right side identities.

The DFF physical implementation must enforce:

```text
D                  → M_DATA.RepeaterRear
C                  → M_LOCK.RepeaterRear
C                  → INV_C.TorchInput
M_DATA.Front       → S_DATA.RepeaterRear
INV_C.Output       → S_LOCK.RepeaterRear
M_LOCK.Front       → M_DATA.RepeaterSide
S_LOCK.Front       → S_DATA.RepeaterSide
S_DATA.Front       → Q
```

The two lock connections are control relations rather than ordinary signal-flow edges.

### 7. Legacy fallback cannot compile a DFF

`compile()` currently tries the unified planner and falls back to the legacy row/channel compiler when planning fails.

The legacy emitter has no Design H implementation. A stateful compile must not silently fall back to it.

Recommended policy:

```text
if netlist contains a stateful gate:
    use only the stateful-capable planner
    return an explicit physical/stateful compile error on failure
else:
    preserve the existing planner → legacy fallback behavior
```

## Recommended implementation phases

### Phase 1: fixed Design H macro

Do not initially let all five primitives move independently.

Create one simulator-verified local-coordinate template with four horizontal rotations. Do not enable mirroring until the mirrored implementation is separately verified.

Expose only three external ports:

- D input
- C input
- Q output

Implement the internal signal paths and side locks entirely inside the macro.

This is substantially smaller and easier to certify than immediately teaching the generic router to construct arbitrary locking relationships.

### Phase 2: compiler integration

1. Allow `DffPosedge` through compile validation.
2. Give the DFF a stateful macro body during placement.
3. Reserve its complete footprint and keep-out region.
4. Route only external D, C, and Q nets.
5. Prevent stateful circuits from entering legacy fallback.
6. Add structural physical verification for the macro.

### Phase 3: functional verification

Run the Design H trace from the existing specification:

```text
D,C = 0,0
C↑
C↓
D↑
C↑
D↓
C↓
C↑
```

Expected Q:

```text
?,0,0,0,1,1,1,0
```

Also verify:

- changing D while clock is high does not feed through to Q;
- a falling edge does not capture D;
- master and slave locks are complementary;
- every supported rotation behaves identically;
- pure combinational feedback still returns `CompileError::CyclicNetlist`;
- feedback crossing a DFF compiles;
- a litematic round-trip preserves behavior.

### Phase 4: real Verilog end-to-end coverage

Add a real Verilog fixture:

```verilog
module dff_example(
    input  wire d,
    input  wire clk,
    output reg  q
);
always @(posedge clk)
    q <= d;
endmodule
```

The test must exercise:

```text
synthesize_verilog
→ lower
→ compile
→ Simulator
```

Do not replace the frontend portion with a manually constructed `GateKind::DffPosedge`.

First confirm that the current `src/frontend/synth.py` pipeline retains the register as `$_DFF_P_`. If plain `abc` rewrites or refuses the sequential cell, adjust the Yosys passes rather than parsing Verilog inside REDA.

## Viewer follow-up

After the physical compiler works:

1. Register a Verilog fixture containing a DFF.
2. Expose D and clock as viewer inputs.
3. Add Play/Pause or a fixed tick-rate runner.
4. Implement playback by repeatedly calling `Session::step()`.
5. Do not use `run_until_stable()` as the primary operation for an oscillator.

`run_until_stable()` remains useful for finite state transitions. An autonomous oscillator will intentionally end in `SimulationError::Diverged` when passed to that API.

## Autonomous redstone clock: separate feature

Completing DFF support enables synchronous sequential logic but does not generate a clock.

For a self-running circuit, add an explicit primitive such as:

```text
Clock(period)
```

or a verified baked oscillator macro.

Do not initially admit arbitrary combinational cycles because:

- combinational-loop Verilog has no ordinary synchronous meaning;
- oscillator period depends on physical routing delay;
- polarity assignment, timing, placement, equivalence, and other passes assume a combinational DAG;
- broadly relaxing cycle checks risks infinite graph walks and nondeterministic compilation.

## Acceptance criteria

The feature is complete when all of the following hold:

1. Real `always @(posedge clk)` Verilog produces a recognized DFF through Yosys.
2. A DFF feedback cone survives lowering.
3. The physical world contains four repeaters and one clock inverter per DFF.
4. Lock repeaters terminate at data-repeaters' side ports, never their rear ports.
5. The simulator passes the complete capture trace.
6. Every supported horizontal rotation passes the same trace.
7. Pure combinational feedback remains rejected.
8. A stateful compile failure does not silently fall back to the legacy emitter.
9. The viewer can step or play the compiled sequential circuit.
10. The emitted litematic is verified in Minecraft Java 26.2.

## Bottom line

The frontend, gate model, lowering, and primitive graph already contain most of the sequential foundation. The principal missing work is:

1. a fixed physical Design H realisation;
2. typed repeater side-lock placement/routing;
3. stateful-aware physical certification;
4. removal of the invalid legacy fallback for stateful circuits.
