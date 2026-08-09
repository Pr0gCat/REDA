# Sequential logic

## Why this is the next thing

Everything the compiler has gained recently has been quality: the decoder went
from 348,348 blocks and 166 ticks to 8,130 and 70, verified in a real 1.20.1
server. Capability has not moved. `CompileError::CyclicNetlist` rejects feedback,
so there are no registers, no clock, no memory.

A computer needs state. A fast decoder is still not one.

**Target: a four-bit register that holds a value, verified in the real game** —
the same shape of goal as the seven-segment decoder, concrete enough to be
either done or not.

## What Yosys actually gives us

Measured, not assumed:

```
reg4  synth   {$_DFF_P_: 4}
reg4  +abc    {$_DFF_P_: 4}                              <- abc does not touch it
cnt   synth   {NOT:1, AND:1, NAND:1, XOR:2, XNOR:1, $_DFF_P_: 4}
cnt   +abc    {NOT:6, NOR:9, $_DFF_P_: 4}
```

(The `+abc` rows were taken when `synth.py` still ran `abc -genlib
redstone_nor.genlib`, which is why `cnt` comes out as NOT/NOR. Plain `abc`, what
runs since `7bb3155`, leaves Yosys's default gate set instead. The finding these
lines are here for is unaffected: the four `$_DFF_P_` pass through either way.)

A flip-flop is a **primitive cell** in Yosys, not a composition of gates. abc is
combinational and optimises only the logic *between* flip-flops; the flip-flops
pass through untouched.

This mirrors real flows, where a flip-flop comes from the standard cell library
as a designed cell — nobody synthesises one from gates, because the result is
worse and timing closure depends on the library cell's characterised behaviour.

**So the flip-flop is a topology library entry**, sitting in `YOSYS_CELL_KINDS`
beside `NOR2`. That is precisely what the library is for, and it is the second
time the standard-cell analogy has told us where something belongs.

## Cycles: sharpen the check, do not delete it

A netlist with registers has cycles — `q → logic → q` runs through a flip-flop.
But cut at the sequential elements and it is a DAG again.

So the rule is not "allow feedback". It is:

> **A cycle is legal when it passes through a sequential element. A purely
> combinational loop is still an error.**

That second half must stay. A combinational feedback loop in redstone is an
oscillator, and it is a real design mistake that this check catches.

Same shape as the merge relaxation in `ac98e35`: the condition gets precise, the
check does not get weaker.

### Two graphs, two different answers

- **Gate level** — a DAG once cut at sequential elements.
- **Primitive level** — a flip-flop's own topology contains a genuine
  cross-coupled loop, so cycles exist inside a single cell.

The invariants work at different levels and must each be told which answer
applies to them. Treating the primitive graph as acyclic would reject every
register; treating the gate graph as freely cyclic would accept oscillators.

## Level-sensitive first, edge-triggered from two of them

Yosys asks for `$_DFF_P_`, which is edge-triggered. Edge detection in redstone
needs a pulse generator, which is expensive.

The standard construction avoids it: **a master-slave pair of level-sensitive
latches is an edge-triggered flip-flop**, which is how silicon does it too. So
the library's `$_DFF_P_` entry is two latches, and the latch is the thing worth
getting right.

For the latch itself, two redstone realisations are worth having:

- **Cross-coupled NOR torches** — buildable with what exists today; the only
  blocker is the cycle rule above. The cheapest path to proving feedback works.
- **Repeater locking** — smaller and faster, but needs the side ports that G5
  (typed ports) would add.

Start with the first. The second becomes the second entry for the same gate
kind, which finally exercises "one gate type, several realisations" on something
that matters.

## The clock is an input, not something we emit

A redstone clock is a torch loop or a repeater loop. The compiler should **not**
build one; it should expose the clock as an input port.

Two reasons, and the second is the one that matters:

- A test bench can drive it. The RCON harness already sets inputs and reads
  outputs; a clock pulse is set-high, settle, set-low, settle.
- **It keeps `run_until_stable` meaning what it means.** A synchronous design
  with the clock held constant does settle. Emitting a free-running oscillator
  would be the only thing in the output that never stops, and it would break the
  definition every measurement in this project rests on.

My earlier claim that "settle" needs redefining was wrong. Externalise the clock
and it does not.

## The timing model does need to change

`2 * (gates + repeaters on the critical path) + lamp` is asserted exact on all
five circuits and assumes a DAG from input to output.

For sequential logic the question is different: **the longest combinational path
between two sequential elements**, because that sets the minimum clock period.
Input-to-output settle stops being the interesting number.

The existing model should keep working for combinational circuits — it is the
instrument that has caught several real errors — and gain a register-to-register
form alongside it, not be replaced.

## Verification

The RCON harness needs no new mechanism: set D, pulse the clock, read Q, compare.
`conformance/circuit_conformance.py` already drives any circuit `mc_dump` can
describe, polls to quiescence rather than sleeping a guess, and distinguishes an
environment failure from a circuit one.

What it does need is a notion of an input sequence rather than a single vector,
since a register's behaviour is only observable across time.

## Debt this phase should probably absorb

`compile()` materialises every netlist input as a lever. That conflates an input
port with a test fixture and makes two compiled circuits impossible to join.

**A register is the first thing that genuinely wants to be reused**, so this is
where the cost stops being theoretical. Worth doing here rather than deferring
again.

## What must not regress

Re-measured 2026-08-09 at `f70ef0e`, with
`cargo test --release --test reference_circuits -- --nocapture` and
`cargo test --release --test verilog_frontend -- --nocapture`:

| circuit | gates | blocks | settle |
|---|---|---|---|
| and4 | 7 | 472 | 24 |
| full_adder | 22 | 1784 | 62 |
| segment_a | 46 | 6416 | 82 |
| seven_segment | 84 | 16244 | 112 |
| seven_segment (Verilog) | 56 | 12348 | 88 |

The four hand-written rows are unchanged from when this was written. The Verilog
row is not: it read `37 | 8130 | 70` here, which was the ABC-technology-mapped
decoder. ABC no longer maps (`7bb3155`), and the gate-level netlist that
replaced it lowers to a larger circuit. That is a known open regression, not a
new floor to defend — see `2026-08-09-polarity-assignment.md`, whose bar is
`99107f4`'s measured 31 gates / 7888 blocks / 82 ticks.

Four invariants stay: spacing, connectivity, torch merge, signal strength. None
may be weakened to admit a register — and if one fires, it is naming what is
wrong.

## Order

1. **The cycle rule.** Independently testable: a loop through a sequential
   element compiles, a purely combinational loop still fails, and the same
   netlist shape distinguishes them.
2. **The latch**, as cross-coupled NOR torches, with a library entry.
3. **`$_DFF_P_` as a master-slave pair**, and the frontend mapping for it.
4. **Register-to-register timing**, alongside the existing model.
5. **A four-bit register verified in a real 1.20.1 server**, driven by a clock
   the harness pulses.

## Out of scope

- Pistons, and therefore quasi-connectivity. Latches do not need them, and QC is
  the hardest part of redstone to model correctly.
- Placement as optimisation. Sequential logic will stress layout in new ways —
  feedback loops, timing closure — and building the placer before knowing those
  requirements would repeat `wip/3d-placement`'s mistake.
