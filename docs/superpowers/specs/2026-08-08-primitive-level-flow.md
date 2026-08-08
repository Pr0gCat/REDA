# A primitive-level flow with a force-directed planner

## What is being replaced

`compile()`'s backbone has not changed since the compact-layout work. Every
improvement since has been made inside it:

```
build_floorplan   ASAP levelisation into rows, barycentre ordering
reserve_columns   north-south columns
assign_tracks     left-edge; one edge gets one track
layout_z          where each track sits (now spread across Y instead of Z)
resolve_bypass    a special case for short hops
emit
```

The model is rows, channels and tracks: **an edge is assigned a lane, and
travels it.** Dissolving the gate cell removed the boundary between a gate's
interior and its exterior, but the router underneath is the same one. Spreading
tracks across Y instead of Z changed which axis pays for separation, not what
is being separated.

The measurements all say the same thing about it. 3.9% of the bounding volume
is occupied. 0.75 air cells sit under the average block, purely because that
block is propped up. 46 of 156 edges take the direct route; the other 110 climb
to a lane and back because that is the only path the model offers. None of
those are inefficiencies in the router — they are what the model is.

## The flow

```
netlist                gate-level graph: gates and the signals between them
   |                   (Yosys/ABC output, or a hand-built Netlist)
   |
   |  gate topology    how one gate is realised in primitives.
   |                   Not a fixed template -- a choice, and an
   |                   optimisation point.
   v
primitive graph        one flat graph of redstone primitives for the whole
   |                   circuit. No gate boundaries left.
   |
   |  passes           simplification and optimisation at primitive level
   v
placement              force-directed: connected primitives pull together,
   |                   collisions push apart
   v
legalisation           continuous positions become a legal block layout
   v
world
```

## The primitive level

The analogue of expanding a CMOS gate into its P and N transistors. A redstone
gate expands into the things the simulator actually models:

- **torch** — the only element with a function: dark when its support is
  powered
- **dust** — carries a strength that decays with distance
- **repeater** — restores strength, costs a tick, one-way
- **solid block** — carries power to what touches it, supports what stands on
  it

Everything the compiler emits is one of these. Once a circuit is a graph of
them, there is no such thing as "inside a gate".

### The topology library

Gate topology is a **library**: gate type to primitive graph. `NOT` maps to
`input — torch — output`; each NOR arity maps to its own graph. It is written
once and consulted, not derived per gate.

This is a standard cell library that stops at connectivity instead of going all
the way to layout. Substituting each gate in the netlist for its library entry
and stitching the signals together is what flattens the circuit into one
primitive graph — a mechanical pass, not a decision.

Making it a library is what makes it an extension point. A redstone technique
someone else discovered is a better entry for some gate type, added without
touching the compiler — which is what this project set out to make possible in
the first place, one level lower than originally aimed.

A gate type may have more than one entry. Which is cheapest depends on where
its signals come from, so the choice cannot be finished before placement
begins; the first version may pick by rule. What must hold from the start is
that **the library admits alternatives**, so choosing between them later is a
change of policy, not of architecture.

### Topology carries no positions

A gate topology is **connectivity only**. A NOT gate is
`input — torch — output`. Nothing in it says which face, which axis, which
coordinate. Every position is the planner's to choose, and the topology is the
same graph whether the result ends up flat or stacked.

This is the separation that makes the rest of the flow work: the same topology
can be realised in many geometries, so exploring geometry never means rewriting
the logic.

### Nothing physical lives here either

Support blocks, faces, orientations, isolation, the choice of whether two
torches share the block that powers them — none of that is topology. A torch is
a torch. How it is built out of blocks is the planner's problem, and keeping it
out of this layer is what lets the same graph be realised many ways.

That also means a real optimisation like *two gates sharing one support block*
is a planning decision, not a graph rewrite. It does not belong to the passes
described below.

### Rigid and routable, at realisation time

The planner does need to know that some connections must become physical
adjacency while others become a dust path of any length. That is not an
annotation on the graph — it follows from the primitive types at each end, and
the planner reads it off them.

It matters because classic cell placement has only the second kind: a net is a
wire and the router takes it wherever. Here much of the graph will realise as
rigid, which makes this closer to embedding a partly-rigid graph in a lattice
than to placing cells. A spring system that treats both alike will pull the
rigid parts apart and hand legalisation an impossible problem.

## Passes on the primitive graph

The reason to flatten is that opportunities invisible at gate level become
local rewrites here: two gates whose supports could be the same block, a dust
run that is really the tail of another, a repeater that exists only because a
route was longer than it needed to be.

This is the same shape as the rewrite-pass idea the project started from, one
level lower than it was originally aimed. The passes are where redstone
techniques discovered by other people can enter as composable rules rather than
special cases in the router.

Nothing here requires equality saturation on day one, but the representation
should not rule it out.

## The planner

Force-directed placement: routable edges pull their endpoints together,
occupancy pushes apart, adjacency edges are constraints rather than forces.

Two things decide whether this works, and both are the whole difficulty:

**Legalisation.** A spring system settles at continuous positions. Blocks live
on a lattice, cannot overlap, and adjacency edges must land exactly. Turning a
relaxed layout into a legal one without destroying what the relaxation achieved
is the hard, classical part of analytical placement, and it is harder here
because of the rigid edges.

**What the springs minimise.** The objective should be delay, and unusually we
can compute delay without simulating: `src/timing/`'s model
(`2 * (gates + repeaters on the critical path) + lamp`) is asserted exact on all
five circuits. Repeaters follow from wire length over 15, so minimising
critical-path wire length is minimising delay — the classic objective, arrived
at from our own measurements rather than borrowed.

Total wire length is the secondary objective; it governs size.

The diagnostics we now collect — fill ratio, air-below-block, per-part repeater
breakdown — are **not** objectives. Each has a degenerate minimum that a
planner would happily find. They are how we understand a result, not what we
ask for.

## Choosing topologies, and the loop that closes

Which library entry is cheapest for a gate depends on where its signals come
from, which is a placement result. So the choice cannot be made well before
placement, and placement cannot revisit it afterwards unless something carries
the information back. That loop is the missing piece, and it is the same
technology-mapping-versus-placement problem physical synthesis has always had.

**Iterative.** Expand with a default entry, plan, then ask each gate whether a
different entry would be cheaper given where its neighbours actually landed,
and re-plan. Repeat while it is still paying.

Two things this requires of the representation, both free now and expensive to
retrofit:

- **The flat graph must remember which primitives came from which gate**, so a
  region can be re-expanded without rebuilding everything.
- **Library entries may carry embedding hints** — relational preferences like
  "these two inputs want to be on opposite sides", "this output wants to be
  coplanar with that input". Not coordinates; constraints on the embedding,
  read by the planner as soft terms. Some real redstone patterns only make
  sense with a particular relative arrangement, and a library that cannot say
  so can only hold the arrangement-agnostic ones.

## Effort and cost

The algorithm is steered by a **cost function with weights** and bounded by an
**effort budget**.

```
cost = w_delay * (critical-path repeaters)
     + w_size  * (total wire length)
     + ...
```

Four things decide whether this is usable rather than decorative:

**Terms must be normalised.** Ticks and blocks are different units on different
scales. Weighting raw terms makes the weights meaningless numbers that need
re-tuning for every circuit. Normalise each term — against a baseline, or its
own achievable range — so a weight means the same thing everywhere.

**Effort is a budget, not a mood.** It should map to something countable:
iterations, cost evaluations, or wall-clock. And the loop must be **anytime** —
interruptible at any moment with a legal result in hand, just a worse one than
if it had run longer. There is never a state where stopping early yields
nothing.

**Evaluation has to be incremental.** If scoring a candidate means recompiling,
the loop manages a handful of iterations. The delta cost of a single move is
what makes an iterative planner viable at all, and it constrains what can go in
the cost function: a term nobody can evaluate incrementally is a term the loop
cannot use.

**The result must be reproducible.** Given the same input, weights, effort and
seed, the output must be identical. An optimiser whose output drifts between
runs cannot be bisected, compared against a baseline, or blamed for a
regression — and every measurement in this project so far has depended on being
able to do exactly that.

Default weights are **measured, not chosen**. Sweep them the way `BAND_CAP` was
swept and record the table in a doc comment, so the next person does not re-run
an experiment we already ran.

The diagnostics — fill ratio, air-below-block, per-part repeater breakdown —
stay out of the cost function. Each has a degenerate minimum a planner would
find immediately. They explain a result; they are not what is asked for.

## Correctness

Four invariants run unconditionally in `compile()`: spacing (keep-out derived
from `connectivity::dust_reach`), connectivity (actual equals intended), torch
merge (each gate's geometry really implements a NOR), and signal strength (every
net arrives above zero).

They are **constraints, not cost terms**. A layout that violates one is not
expensive, it is illegal. Folding correctness into a penalty weight produces a
planner that is usually right, and this project has spent enough time on things
that were usually right.

They also become the reason this is attemptable at all. The previous attempt at
a structural change of this size — Y-stacked placement, `wip/3d-placement` —
failed on two nets' dust merging and took hours of gate-by-gate comparison to
diagnose. The same collision now reports its own coordinates.

## What must not regress

`02110dd` through `7b1618d` are the current best, verified in a real Minecraft
1.20.1 server:

| circuit | gates | blocks | settle | fill | air/block |
|---|---|---|---|---|---|
| and4 | 7 | 472 | 24 | 2.7% | 0.46 |
| full_adder | 22 | 1784 | 62 | 2.9% | 0.70 |
| segment_a | 46 | 6416 | 82 | 3.3% | 0.75 |
| seven_segment | 84 | 16244 | 112 | 3.9% | 0.75 |
| seven_segment (Verilog) | 37 | 8130 | 70 | — | — |

The truth tables and the RCON conformance run are the floor. A faster, smaller
circuit that fails either is not a result.

## Order

Each step lands with its own measurement, and one that does not pay for itself
is reverted rather than carried.

1. **The primitive graph and the expansion into it**, with the two edge kinds
   distinguished. Verifiable on its own: expanding today's netlists and
   re-emitting today's geometry must produce byte-identical worlds.
2. **The planner**, replacing rows and channels. The largest step and the one
   most likely to need more than one attempt.
3. **Legalisation**, which cannot be separated from 2 in practice but should be
   a separate, testable component.
4. **Passes on the primitive graph.** Deliberately last: they optimise a
   representation, and the representation should be settled and measured first.

## Out of scope

- Logic optimisation above the gate level. Yosys and ABC do that.
- VHDL. The seam is Yosys's JSON.
- The viewer, beyond keeping it able to draw what the compiler emits.
