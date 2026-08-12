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

**Springs encode plain wirelength, so delay is not in the objective.** Not the
15-cell step, and not criticality-weighted stiffness. This is worth stating
against the founding thesis rather than around it: §2 says delay in redstone is
a placement problem, and this design does not optimise delay. It optimises wire
and then reports what delay it got. §66 is right that the real objective is "every
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

It applies **between conductors of different nets**, which is the rule as
written, and not between every pair of bodies. Getting that wrong makes the
design contradict itself: a torch is *required* to touch its own support, so a
projection that pushes all pairs apart fights the weld that holds them
together, and the two take turns undoing each other.

Which means the separation is between **ports, not bodies**. A body does not
sit on a net: a torch's support carries the signal driving it and its torch
carries the signal it drives, and those are different nets by definition -- a
gate is exactly the place one net ends and another begins. Keying the exemption
off a body would exempt a torch from its own output, or separate it from its
own input. Two ports are exempt when they carry the same signal, or when a weld
relates them.

**Room for wires is part of the separation, not a later problem.** Springs pull
and separation pushes, so the relaxed solution sits at exactly the minimum
separation everywhere -- a placement with no corridors. Routes then have
nowhere to go, which is already the observed failure at segment_a and would
become the failure everywhere. Channels exist in the legacy router for this
reason and this design has no equivalent, so the separation carries it: each
body reserves, beyond its own clearance, room for the nets that must reach it.

The reservation is the number of edges that must be *routed* to it -- which is
its degree less the welds, because a torch and its support are adjacent by
construction and no wire runs between them -- times the width one route needs.

That is a first estimate and a measurable one: if placements come out routable
but wasteful, or compact but unroutable, this is the number that was wrong, and
it is one number.

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
  |  snap                         NEW -- quantise facing, re-project, round
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
    /// Half-extents along the body's own axes, from the blocks
    /// `physical::variants` says it occupies. Not a radius: a NOR cell is not
    /// a sphere, and a sphere large enough to contain one wastes exactly the
    /// space this design exists to win.
    half_extent: [f64; 3],
    /// How far the furthest port sits from the centre. Quantising the facing
    /// swings the ports, so this is what the snap margin has to cover.
    port_radius: f64,
}

/// A spring, attached at a port on each end.
struct Pull { from: (usize, PortKind), to: (usize, PortKind), stiffness: f64 }

/// A relation that must hold exactly. Projected, never pulled.
enum Weld {
    OnFace { torch: usize, support: usize },
    BesideAt { lock: usize, data: usize, side: RelativeSide },
}

struct RelaxEffort { iterations: usize, seed: u64 }

fn relax(graph: &PrimitiveGraph, pinned: &PortPlacements, effort: RelaxEffort)
    -> Result<ContinuousPlacement, PlannerError>;
fn snap(placement: &ContinuousPlacement)
    -> Result<Vec<(NodeId, Anchor, u8)>, PlannerError>;
```

### Where the relaxation starts

A spring system with hard constraints is not convex, so the starting point
decides which solution it finds. Starting everything at the origin gives a
knot the projection then has to unpick, and starting at random makes the result
unreproducible unless the randomness is the seed's.

It starts from the placement that exists today: Z by logic depth, X by
barycentre over already-placed sources. That layout is legal, it is what the
current `plan_from_netlist` produces, and it is measurably poor -- 656 blocks
for and4 against the emitter's 472. Relaxation is therefore asked to improve a
known-bad answer rather than to invent one, and the improvement is measurable
against the number it started from.

`RelaxEffort`'s seed perturbs that start. It exists so a run can be repeated
exactly and so a stuck configuration can be retried from a different one, not
because anything in the solve is random.

### Floating point, against a rule that forbids it

`2026-08-11-unified-3d-planner.md` is explicit: "use rational
numerator/denominator pairs or integer cross multiplication for normalisation,
**never floating-point ordering**", and `NormalisedScore` implements exact
rational comparison to obey it. This design is `f64` throughout, so it has to
say why that is not the same thing.

The rule is about **ordering candidates**. Two layouts whose scores differ in
the last bit must not swap places depending on how they were summed, or the
search stops being reproducible and every measurement taken from it is noise.
Nothing here orders anything: the relaxation solves, `snap` quantises, and what
leaves the module is integer anchors and one of four facings. Floats are the
solver's internal state, not a comparison key.

That distinction holds for one build. It does **not** hold across two, and this
project has two: `viewer/` compiles the same crate to wasm, and once `compile()`
uses this placer, the circuit drawn in a browser is the circuit this code
placed. If wasm and native disagree in the last bits, the same netlist yields
two different layouts and the viewer stops being evidence about the compiler.

So one test decides it: place a reference circuit natively and under wasm and
require identical anchors. If they agree, floats stay. If they do not, the
positions become fixed-point -- the arithmetic is addition, multiplication and
comparison, all of which fixed-point does exactly -- and the only thing lost is
the convenience of `f64` in the projection.

This is stated as a risk with a test rather than settled here, because which
way it goes is a fact about two toolchains and not a matter of design.

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

Four terms, each with a source:

1. each body's half-extents, from the blocks `physical::variants` says it
   occupies;
2. two cells of conductor clearance, from
   `2026-08-09-channel-safety-condition.md`, between different signals only;
3. room for the nets that must reach the body -- its degree times one route's
   width, so that a relaxed placement has corridors rather than only clearance;
4. a rounding margin, derived below.

### Why `snap` quantises the facing first

An earlier draft claimed the margin could be one cell, on the grounds that
rounding moves a body half a cell and two bodies therefore approach by at most
one. That reasoning covers translation and ignores rotation, which is the
larger effect: quantising a facing moves it by up to 45 degrees, and a body's
blocks and ports sit away from its centre, so they swing by up to
`port_radius * 2 * sin(22.5°)` -- most of a cell for a repeater, more for
anything larger. A margin of one does not cover it and `snap` would silently
hand on a placement the projection had already made legal.

So `snap` is three steps, not one:

1. **Quantise every facing** to the nearest of the four, and recompute each
   body's occupied cells at that facing.
2. **Project again**, in continuous space, with the facings now fixed. This
   repairs whatever the rotation broke, and it is the same projection the
   relaxation was already running.
3. **Round the positions**, with a margin of one cell -- which now covers what
   it was always claimed to cover, because after step 2 nothing is rotating.

`snap` returns a `Result` because step 2 can fail: quantising every facing at
once can produce a configuration with no legal repair, and that is a real
outcome to report rather than round anyway.

### What `snap` hands back, and where primitives stop being primitives

Relaxation moves primitives. `PlanCandidate` is indexed by *gate*: one anchor
per gate followed by one per primary input, and `emit_primitives` reads
`netlist.gates[index]` to decide what to place there.

Today that mismatch is not one. Every realisable gate is a single torch or a
wire merge, so primitive and gate correspond one to one and `snap`'s `NodeId`
is a gate index by coincidence rather than by design. It becomes a mismatch the
moment a gate expands to several primitives -- Design H's five, or any macro
cell -- and at that point `PlanCandidate` has to carry primitives rather than
gates, and `snap`'s return type stops fitting.

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
| facings cannot be quantised | `snap` step 2 finds no legal repair | `FacingsDoNotQuantise`, naming a body whose four facings all conflict |
| violation survives `snap` | checked directly after rounding | the margin argument is wrong; report the pair rather than let an invariant find it later |

The last is a principle: the invariants exist to catch real errors, not to
catch the legaliser's leftovers.

`RelaxEffort` carries an explicit seed. No clock, no unseeded randomness.

## Testing

Cheapest first, each testing one claim:

1. **Determinism.** Same graph and effort, identical placement, bit for bit.
2. **Orientation.** A hand-built pair: a torch whose only consumer sits to its
   east ends up facing east. This is the claim that torque produces
   orientation, on a case small enough to verify by hand.
3. **Separation after snap.** For every reference circuit, no two primitives of
   different signals are closer than the rule allows -- checked directly on the
   snapped anchors, not through the invariants. This is the claim the margin
   makes, and it is the one an earlier draft got wrong by forgetting rotation,
   so it is tested on a case that rotates: a body whose relaxed facing sits
   near 45 degrees, where quantising moves it furthest.
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
