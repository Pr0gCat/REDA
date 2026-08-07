# 3D placement: stack logic levels vertically

## The observation this rests on

Minecraft's simulation-distance limit is **horizontal**. A circuit wider than
roughly 1040 blocks has parts sitting in unloaded chunks where redstone does not
tick, which is what sank MinecraftHDL's version of this same decoder.

The world is 384 blocks tall and nothing about height costs loading. The current
layout is 232 x **5** x 298 — it fights for horizontal space while using 1.3% of
the axis that is free.

## What the current layout does

ASAP levelisation puts each logic level in its own row. Rows are laid out along
Z, gates within a row along X, and the channels that carry signals from one row
to the next occupy the Z space between rows. 38 tracks at `TRACK_SPACING = 5`
are most of `seven_segment`'s Z extent of 298.

So consecutive logic levels — which by construction talk to each other, and
which carry the critical path — are separated by whatever Z the channel between
them needs. Every hop on the critical path pays that separation.

## The change

Levels stack along **Y** instead of spreading along **Z**.

Level *i* sits in its own horizontal band. Between band *i* and band *i+1* is the
routing channel that connects them. Consecutive levels become **vertically
adjacent**: the hop from a gate to its consumer is a short climb plus a short
horizontal run, rather than a traverse across the full Z extent.

This is the same reason chips have many metal layers rather than one enormous
one, and it is why the wire-length scaling argument (average distance going from
√N to ∛N) applies at all — that scaling only materialises if the placement
actually uses the third dimension for connectivity, not just for stacking
unrelated things.

## Why this compounds with the dust-staircase change

Once ramps are dust staircases rather than repeater ladders, vertical movement
costs **signal strength, not time**. A climb of four blocks costs four strength
out of fifteen and zero ticks.

That inverts the usual trade. In a 2D layout, moving between the gate plane and
the track plane was the single most expensive part of every hop. With staircases
it becomes nearly free, and a design that moves vertically *often* stops being
expensive and starts being the cheap option.

The two changes have to land in that order for this reason. Doing 3D placement
on top of repeater ramps would multiply the number of expensive climbs.

## What has to be worked out

**Band height.** Each level needs its gate plane, an isolation layer, and its
share of routing. The current 2D design uses 5 Y for one level's worth of that.
Whatever the band height turns out to be, it multiplies by the level count, so
it is the number that decides whether this fits in 384.

**Edges that skip levels.** A netlist edge does not always connect consecutive
levels; a signal produced at level 0 may be consumed at level 8. Those need
vertical channels that pass through intervening bands without interacting with
them. This is the part with no equivalent in the current design and the part
most likely to be underestimated.

**Isolation between bands.** The `Y=2` floor exists because redstone's climb and
diagonal-descend rules bridge layers that look separated on a plan view. Stacking
many bands multiplies every surface where that can happen. The isolation
discipline that works for two layers has to be shown to work for many, not
assumed to.

**Strength budget across a climb-heavy route.** Staircases spend strength.
A route that climbs several bands spends it several times. The budget arithmetic
that governs repeater placement has to account for a route's total climb, not
just its horizontal length.

## How it gets judged

The delay model validated in
`docs/superpowers/specs/2026-08-07-routing-cost-breakdown.md`:

```
settle ticks = logic_depth_bound + 2 x (critical-path repeaters) + 2
```

and the per-part breakdown from `routing_cost_report`. Both existed before this
change and both must be reported after it, per circuit, so the effect is
attributable rather than merely observed.

The horizontal extent must stay under the ticking-area bound that
`tests/seven_segment.rs` already asserts. The vertical extent gains a bound of
its own: the build must fit between Minecraft's world floor and ceiling with
room to place it somewhere sensible.

## Out of scope

- Gate-entry cost (the mandatory socket-terminating repeater and the corner
  turn). It is the largest remaining fixed tax after dust staircases land, and
  it deserves its own measurement and its own task rather than being folded into
  a placement rewrite.
- Any change to the netlist, the cell library, or the simulator.
