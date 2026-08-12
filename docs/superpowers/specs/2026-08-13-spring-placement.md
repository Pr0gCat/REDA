# Spring Placement

**Status:** design, not implemented.

`2026-08-05-redstone-eda-design.md` §7.1 left one question open and asked for
an experiment before answering it:

> **待決策**：global placement 走解析式還是離散搜尋。這需要在階段 D 之前以小
> 規模實驗決定，不在本文件定案。

The experiment was never run. What exists instead is a discrete local search
that neither branch of that question proposed, and this document is what
measuring it produced.

## What the measurement says

`optimise` offers each primitive six one-cell translations and accepts what
scores better and verifies. On 2026-08-12, over 168 verifications:

| circuit | gates | legal proposals |
|---|---|---|
| and4 | 7 | 6 of 24 |
| full_adder | 22 | **0 of 24** |
| segment_a | 46 | **0 of 24** |
| seven_segment | 84 | **0 of 24** |

Zero. Not slow, not under-budgeted: there is nothing in the search space to
find. The row/channel/track router packs every row to its own spacing limit,
so one gate out of line breaks them, and every single-cell move is illegal by
construction.

and4 improving -- 472 blocks to 405, settle 18 to 16 -- was read at the time as
"the mechanism works, only scale is missing". It is not. and4 is the smallest
circuit and the only one loose enough to have slack.

A second measurement points the same way. The planner's own placement, when it
places at all, is worse than the router it is meant to replace:

| and4 | blocks |
|---|---|
| row/channel/track | 472 |
| planner's own placement | 656 |

The legacy emitter is not a stopgap that happens to work. Its row and channel
model encodes real spacing knowledge, and "rows by logic depth, columns by
barycentre" does not have it. That is the thing to replace, and replacing it
means having knowledge of its own, not having freedom.

## The decision

Global placement is **analytical**: a continuous relaxation, legalised onto the
lattice afterwards.

This is the branch §7.1 flagged as newly plausible -- VPR made analytical
placement its default in 2026-06 for roughly 10% wirelength, and LogicLoom, the
only large-scale in-game auto-router with published behaviour, uses
force-directed placement. It is also the branch this project has no evidence
against, the retracted argument in §7.1 having been the only one.

Five decisions follow, each with the alternative it was chosen over.

**Bodies are primitives, not gates.** A torch, a repeater, a lever and a lamp
each move on their own; a cell is a soft spring holding its members together,
not a rigid body. The cost is that geometry a topology template used to
guarantee -- a torch on its support's face, Design H's lock repeater at the
data repeater's side -- has to become an explicit constraint. The gain is that
`physical.rs`'s typed ports become the thing placement is expressed in.

**Springs attach to ports, not to centres.** Every edge of the primitive graph
lands on a specific port, and `physical.rs` already says where each port sits
relative to its primitive and which way it faces. Attaching there means the net
force gives a position and the net *torque* gives a facing. Orientation stops
being a separate problem, or -- as it is today -- a constant: `place_nor_gate`
faces north unconditionally.

**Springs encode plain wirelength.** Not the 15-cell step, and not
criticality-weighted stiffness. §66 is right that the real objective is "every
hop on the critical path stays within 15 cells" rather than total wirelength,
but a short-wire placement satisfies that constraint most of the time, the
convergence behaviour of a plain quadratic system is known, and a non-monotone
force field is hard to debug when it does not settle. Hops that end up over 15
get a repeater, which is what repeaters are for. If the constraint turns out to
bind often, weighting comes later, with the measurement that says so.

**The spacing rule is the counterforce, and it is a hard constraint.** Not
pairwise electrical repulsion, and not a bin density term. Two conductors of
different nets need two cells of clearance -- derived in
`2026-08-09-channel-safety-condition.md` from `dust_reach`, and enforced today
by `verify_spacing`. That number is not a tuning parameter, so it is projected
after every step rather than added as a force. The continuous solution is then
nearly legal by construction, which is the whole reason to prefer it: the
solver already knows what legalisation will ask of it.

**Everything switches, including `compile()`.** Legacy stays as a comparison,
not as the production path. The risk is stated plainly below.

## Architecture

Two new stages between things that already exist:

```
Netlist
  |  primitive_graph::expand      exists -- primitives, typed edges, ports
PrimitiveGraph
  |  relax                        NEW -- continuous, spacing projected each step
ContinuousPlacement
  |  snap                         NEW -- round to lattice, quantise facing
PlanCandidate                     anchors and variants, not yet routed
  |  route_every_net              exists -- A* with rip-up
  |  realise_and_verify           exists -- blocks, then four invariants
CompiledCircuit
```

`plan_from_netlist` keeps its netlist and `PortPlacements` arguments; its body
becomes expand, relax, snap. Routing, realisation and verification are
untouched, so a failure after placement is a failure of something already under
test.

## Components

A new module, `src/compile/relax.rs`:

```rust
/// One primitive, in continuous space.
struct Body {
    node: NodeId,
    kind: Primitive,
    position: [f64; 3],
    facing: f64,        // radians; quantised to four at snap
    extent: f64,        // from physical::variants' blocks
}

/// A spring, attached at a port on each end.
struct Pull { from: (usize, PortKind), to: (usize, PortKind), stiffness: f64 }

/// A relation that must hold exactly. Projected, never pulled.
enum Weld {
    OnFace { torch: usize, support: usize },
    BesideAt { lock: usize, data: usize, side: RelativeSide },
}

struct RelaxEffort { iterations: usize, seed: u64 }

fn relax(graph: &PrimitiveGraph, effort: RelaxEffort)
    -> Result<ContinuousPlacement, PlannerError>;
fn snap(placement: &ContinuousPlacement) -> Vec<(NodeId, Anchor, u8)>;
```

### What `physical.rs` has to gain

It gives four discrete facings per primitive, each with typed ports and the
blocks it occupies. Relaxation needs port offsets at an *arbitrary* angle.

The addition is small: treat facing 0 as canonical and rotate. The four
variants become the four quantised cases of one layout, so the continuous stage
uses the canonical version and `snap` looks the rotated result back up among the
four. The module stops being dead code and becomes the thing both stages share.

### Resolving a port from an edge

`primitive_graph`'s edges name nodes, not ports, but the port is determined by
the primitive's kind and the edge's kind:

- a torch's input is its support (`TorchInput`), its output is the torch
  (`TorchOutput`);
- a repeater reads at the rear (`RepeaterRear`) and drives the front
  (`RepeaterFront`);
- `EdgeKind::RepeaterLockSide` resolves to `RepeaterSide`, and produces a
  `Weld` rather than a `Pull`.

`PortKind` already enumerates exactly these. This is a lookup, not a new idea.

### The separation the projection enforces

Three terms, each with a source:

1. each body's `extent`, from the blocks `physical::variants` says it occupies;
2. two cells of conductor clearance, from `2026-08-09-channel-safety-condition.md`;
3. **plus one**, so that rounding cannot break what the projection established.

The third is the reason `snap` can be arithmetic rather than a repair loop.
Rounding moves a body by at most half a cell, so two bodies can approach by at
most one; a continuous solution separated by the requirement plus one is
therefore still separated after rounding. Without it, `snap` becomes a second
legaliser with its own failure modes.

### What `snap` hands back, and where primitives stop being primitives

Relaxation moves primitives. `PlanCandidate` is indexed by *gate*: one anchor
per gate followed by one per primary input, and `emit_primitives` reads
`netlist.gates[index]` to decide what to place there.

Today that mismatch is not one. Every realisable gate is a single torch or a
wire merge, so primitive and gate correspond one to one and `snap` maps
straight back. It becomes one the moment a gate expands to several primitives
-- Design H's five, or any macro cell -- and at that point `PlanCandidate` has
to carry primitives rather than gates.

This document does not do that. It notes where the seam is, so that whoever
lands the DFF finds it named rather than discovers it.

### Pinned ports and the shape preference

`PortPlacements` survives unchanged and gets simpler: a pinned port is a body
with infinite mass. It contributes force to its neighbours and takes none, and
`snap` returns it exactly where it was pinned.

`Shape::Tall` does not survive. It packs a level of logic wider than three
gates onto the storey above, which is a heuristic standing in for the thing
relaxation does directly -- spreading in three dimensions because the forces
push that way. Keeping both would mean two mechanisms deciding height, one of
them by a hardcoded three. It is removed when this lands, and the test that
pins it becomes a test that a tall circuit uses more than one level at all.

## Error handling

Each failure is distinct and names itself, in the style `CompileError` already
uses:

| failure | detected by | reported as |
|---|---|---|
| did not converge | budget spent, violation remains | `DidNotConverge { iterations, worst_violation }`, naming the worst pair |
| projection deadlock | no progress for N steps, violation remains | a different error, because the remedy differs: constraints that contradict, not a budget that ran out |
| violation survives `snap` | checked directly after snapping | the "plus one" reasoning is wrong; report the pair rather than let an invariant find it later |

The last is a principle: the invariants exist to catch real errors, not to
catch the legaliser's leftovers.

`RelaxEffort` carries an explicit seed. No clock, no unseeded randomness.

## Testing

Cheapest first, each testing one claim:

1. **Determinism.** Same graph and effort, identical placement, bit for bit.
2. **Orientation.** A hand-built pair: a torch whose only consumer sits to its
   east ends up facing east. This is the claim that torque produces
   orientation, on a case small enough to verify by hand.
3. **Separation after snap.** For every reference circuit, no two primitives
   are closer than the rule allows -- checked directly on the snapped anchors,
   not through the invariants. This is the claim the "plus one" makes.
4. **Welds survive snap.** A torch is on its support's face; Design H's lock
   repeater is at the data repeater's side.
5. **End to end.** Every reference circuit places, routes, verifies, and
   matches its truth table.

## The condition for switching `compile()`

All four hand-written circuits and both Verilog circuits must place, route,
verify, and match their truth tables. Not one fewer.

Beating the legacy emitter is **not** a condition. The choice recorded here is
to switch when it is correct, not when it is better, so a placement that is
larger or slower still ships if it is right. A test that builds both and prints
the comparison keeps that gap visible rather than forgotten.

`the_hand_written_circuits_keep_their_measured_size` pins 472 / 1,784 / 6,416 /
16,244 today. Switching will change them, deliberately, so that commit updates
the expectations to the measured values and records the old ones alongside. The
test's meaning moves from "these must not change" to "these were measured at
this commit, and changing them again needs an explanation" -- which is what it
was always for.

## Out of scope

- **Routing.** The A* and rip-up path stays as it is. Changing placement and
  routing together would leave nothing to attribute a regression to.
- **Weighting springs by criticality.** Timing-driven placement is the obvious
  next question and is deliberately deferred until a measurement says plain
  wirelength misses the 15-cell constraint often enough to matter.
- **The optimiser.** `optimise` keeps its move set. With placement decided by
  physics rather than by a grid, whether local search still has anything to
  contribute is a question for after this lands, and the measurement above says
  it currently does not.
