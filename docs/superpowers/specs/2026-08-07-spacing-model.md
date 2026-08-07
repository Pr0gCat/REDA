# Spacing: routes declare what they need, not just what they occupy

## The mistake this corrects

`seal_cross_talk` is six lines: for each cell to either side of a staircase
step, if it is air, put stone there. It asks nothing else — not whether
anything is nearby, not whether a collision is possible, not whether the cell
will ever be wanted.

It is not carelessness. The router fills the world one edge at a time and, at
the moment it places a step, it cannot know what a later edge will put beside
it. Not knowing, it assumes the worst and walls itself in.

The cost is not the wasted stone. It is that **a route's true footprint is
invisible to whatever decides where routes go.** The router thinks a route
occupies the cells it writes. It actually needs those cells *plus* the space
around them staying electrically clear. Anything that later tries to pack
routes closer — folding, congestion-aware assignment, any placement
optimisation — will reason from the smaller number, conclude two routes fit,
and be wrong.

Post-processing does not fix this. Sealing after the fact leaves the decision
maker working from a footprint that is still too small; it just moves the
failure from "wasted stone" to "no room left to seal". The requirement has to
be known **while routing**, because that is when space is being committed.

## The model

A route occupies cells of two kinds.

**Conductor cells** — dust, repeaters, and the blocks that physically support
them. What the route is made of.

**Keep-out cells** — cells where nothing may be placed that would join this
route's net. Not "cells that must be stone". A constraint, not a material.

The router reserves the union. Two routes may share a region only where
neither's keep-out is violated, and that question is answerable *before* either
is written.

Stone appears only where a keep-out cell is actually threatened by something
that would otherwise connect. Where nothing is near, the constraint costs
nothing.

This is what a spacing rule is in any physical design flow: a wire has a width
and a required clearance, and the router honours both while routing. Nobody
routes first and adds clearance afterwards.

## Deriving keep-out, not declaring it

The keep-out set around a dust cell is **not a fixed shape**. Redstone's climb
and diagonal-descend rules depend on what is above and beside a cell, so which
neighbours could join a given cell's net depends on the blocks already there.

So keep-out has to be computed from the same rules the simulator uses —
`redstone::simulator::connectivity` and `redstone::rules::taxonomy` — read in
reverse: *given a dust cell, which cells would join its net if they held
something conductive?* A hand-written shape would be a second, drifting copy of
the connection rules, and the first time the two disagreed the compiler would
produce a circuit the simulator says is wrong for reasons nothing explains.

## The invariant nothing currently checks

Once keep-out is derived from the connection rules, one more thing becomes
cheap and it is worth more than the stone saved:

> The routed world's **actual** connectivity must equal the netlist's
> **intended** connectivity.

Every verification this project has is end-to-end: simulate, compare a truth
table. When that fails it says "input 5, segment 3 is wrong" and nothing more.
The wide-fan-out failure blocking the Y-stacked placement is exactly a
violation of this invariant — two unrelated nets' dust physically merged — and
finding it took hours of gate-by-gate topological comparison.

With the invariant checked, the same failure reports itself: *the dust at
(x, y, z) connects to net 7, but belongs only to net 22.* That is the
difference between a bug hunt and a diagnostic.

This check belongs in `compile()`, failing loudly, not in a test that only some
circuits happen to exercise.

## Order of work

**On `feat/walking-skeleton`, which is green — not on the 3D branch.**

Introduce the model against the working 2D layout first and show it changes
nothing: the same four circuits, the same truth tables, the same delays. A
spacing model that is correct is *neutral* on a layout that already works, and
proving that neutrality on a known-good baseline is the only way to tell the
model's own bugs apart from the 3D layout's.

Then, and only then, Y-stacking gets retried on top of it. `wip/3d-placement`
holds that work at `5539dae`; its collision is a missing spacing rule, so it is
expected to be a consequence of this change rather than a separate fix.

## Success

- Four circuits, same truth tables, same sizes and delays as
  `f45f6fa` — or *smaller*, where blanket sealing was placing stone nothing
  needed. Report which.
- The connectivity invariant checked inside `compile()`, with a test that
  deliberately violates it and confirms the message names the offending cells.
- No hand-written copy of the connection rules anywhere in `src/compile/`.

## Out of scope

- Y-stacked placement. Separate, and after.
- Folding or any packing optimisation. This makes them possible; it does not
  attempt them.
- Changing gate cell geometry.
