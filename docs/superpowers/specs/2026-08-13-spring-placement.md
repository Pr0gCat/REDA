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

| and4 | blocks | settle |
|---|---|---|
| row/channel/track | 472 | 18 |
| planner's own placement | 572 | 24 |

The legacy emitter is not a stopgap that happens to work. Its row and channel
model encodes real spacing knowledge, and "rows by logic depth, columns by
barycentre" does not have it. That is the thing to replace, and replacing it
means having knowledge of its own, not having freedom.

### The assumption this rests on

Worth naming, because everything below depends on it and it is not proven:
**that the knowledge is the rules, and the structure was only one way of
obeying them.**

What legacy knows is expressed as structure -- rows carrying signal one way,
channels between them, approach columns entering a socket from the side a
terminal can read. The design keeps the last of those, in the router, and
replaces the rest with two rules and a physics: conductors of different nets
stay two cells apart, and a body reserves room for the wires that must reach
it. If that captures what the rows were for, relaxation finds a better
arrangement than a fixed pattern can, because it is not obliged to be regular.

If it does not, the result will be a layout that satisfies both rules and is
still worse, and the honest conclusion then is that the structure *was* the
knowledge -- that rows and channels encode something about redstone that two
pairwise rules cannot say. Test 6 is what asks the question, and it is the
reason the first stage stops before `compile()`: to get the answer before the
expensive half is built.

## The decision

Global placement is **analytical**: a continuous relaxation, legalised onto the
lattice afterwards.

This is the branch §7.1 flagged as newly plausible -- VPR made analytical
placement its default in 2026-06 for roughly 10% wirelength, and LogicLoom, the
only large-scale in-game auto-router with published behaviour, uses
force-directed placement. It is also the branch this project has no evidence
against, the retracted argument in §7.1 having been the only one.

Six decisions follow, each with the alternative it was chosen over.

**Bodies are primitives, not gates.** A torch, a repeater, a lever and a lamp
each move on their own; a cell is a soft spring holding its members together,
not a rigid body. The cost is that geometry a topology template used to
guarantee -- a torch on its support's face, Design H's lock repeater at the
data repeater's side -- has to become an explicit constraint. The gain is that
`physical.rs`'s typed ports become the thing placement is expressed in.

Review added one body that is not a component at all: the block a component
stands on. See "Nothing holds a body up".

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
different nets need two cells of clearance, derived in
`2026-08-09-channel-safety-condition.md` from `dust_reach`. That number is not
a tuning parameter, so it is projected after every step rather than added as a
force.

Worth being exact about who enforces it today, because the design leans on it
and the obvious answer is wrong. `verify_spacing` does **not**: it checks cell
exclusivity, that no cell is claimed by two nets, which is weaker and different.
The two-cell rule is enforced by construction, in the router's keep-out, and a
violation surfaces as `ConnectivityViolation` -- two nets physically joined --
which is the consequence rather than the rule.

So the projection enforces something no single invariant states. That is an
argument for it, not against: placement is where the rule can be satisfied
directly instead of being discovered, downstream, as an electrical accident.

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

That is a first estimate, and the doubt about it is specific rather than
general: a halo is not a channel. Legacy reserves *shared* corridors that many
nets run along; this reserves a private ring around each body, and the corridor
is whatever gaps happen to line up between rings. A high-degree gate gets a
large ring whether or not its neighbours needed one, so the layout can bloat
and still not have a connected path through it.

It is measurable, and it is one number: if placements come out routable but
wasteful, or compact but unroutable, this is what was wrong. If it turns out
that rings cannot produce corridors at all, the honest next move is a term that
knows about regions rather than a bigger ring -- which is the bin density this
design turned down, arriving from the other direction.

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
/// One thing in continuous space. Not always a primitive: a support block is
/// a body too (see below), and it has no node in the primitive graph and no
/// `Primitive` kind, because it is not a component -- it is what a component
/// stands on.
struct Body {
    what: BodyKind,
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

enum BodyKind {
    /// A component the primitive graph named.
    Primitive { node: NodeId, kind: Primitive },
    /// A block something stands on or attaches to. Solid; conducts only when
    /// it is a NOR's support, in which case it carries that gate's input
    /// signal. Signals are the netlist's own names -- `String` -- because
    /// nothing in this codebase interns them and inventing a type here would
    /// be a second way to say the same thing.
    Support { holds: usize, carries: Option<String> },
}

/// A relation that must hold exactly. Projected, never pulled.
enum Weld {
    /// A wall torch on the face of its support.
    OnFace { torch: usize, support: usize },
    /// Design H's lock repeater at the data repeater's side.
    BesideAt { lock: usize, data: usize, side: RelativeSide },
    /// Anything that has to stand on something: dust, a repeater, a lever.
    StandsOn { body: usize, support: usize },
}

struct RelaxEffort { iterations: usize, seed: u64 }

fn relax(graph: &PrimitiveGraph, pinned: &PortPlacements, effort: RelaxEffort)
    -> Result<ContinuousPlacement, PlannerError>;
fn snap(placement: &ContinuousPlacement)
    -> Result<Vec<(NodeId, Anchor, u8)>, PlannerError>;
```

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

### What the springs actually minimise, and what stiffness means

Spring energy is `k * d^2` summed over pulls, `d` measured between the two
ports rather than the two centres. Quadratic, so the unconstrained system is
the classic one and its behaviour is documented; the projection is what makes
it non-convex, not the objective.

Every signal spring has `k = 1`. There is no per-edge weighting, because
weighting is the criticality question deferred above, and a stiffness that
varies without a measurement behind it is the sort of number this project
already spent time removing from the planner.

Cell cohesion is the one exception, and it needs a stated value rather than an
adjective. A cell's members must end up adjacent or the cell is not a cell, so
cohesion is not a preference competing with wirelength -- it is a weld that has
been softened only to let the solver reach the configuration gradually. Its
stiffness is therefore high enough to dominate any single signal pull on the
same body, which for `k = 1` signal springs and a bounded degree means the
maximum degree in the graph. That is a derived number, recomputed per circuit,
not a constant to tune.

Convergence is reached when no body moves more than a tenth of a cell in a
step. A tenth because the rounding margin is a whole cell, so a system still
twitching below that cannot change what `snap` produces; running past it buys
nothing measurable. Reaching `RelaxEffort::iterations` without reaching that
threshold is `DidNotConverge`.

### The separation the projection enforces

Four terms, each with a source:

1. each body's half-extents, from the blocks `physical::variants` says it
   occupies;
2. two cells of conductor clearance, from
   `2026-08-09-channel-safety-condition.md`, between different signals only;
3. room for the nets that must reach the body -- the edges that need routing,
   which is its degree less its welds, times one route's width, so that a
   relaxed placement has corridors rather than only clearance;
4. a rounding margin, derived below.

### Nothing pushes into the third dimension on its own

The obvious reading of everything above is that relaxation will use all three
dimensions, because the forces are three-dimensional. It will not, and the
reason is worth stating before someone builds it and wonders why every layout
comes out flat.

Springs pull along edges and separation pushes along the shortest way out. Both
act in the plane their bodies already occupy, and the starting configuration is
planar -- everything sits at one Y, because that is what the current placement
produces. A planar configuration under in-plane forces stays planar. Height
would never be used at all, and `Shape::Tall` would have been removed in favour
of a mechanism that never did its job.

Two ways out, and this document takes the second.

**Perturb.** Give every body a small random offset in Y at the start, seeded.
Symmetry broken, and where the forces genuinely prefer a second storey they
find one. Cheap, and it makes the seed do something real. But it also means a
circuit that should be flat comes out slightly wrinkled, and every body pays
rounding error in a dimension it did not need.

**Let separation choose the axis.** When two bodies must be pushed apart, the
projection already picks a direction; today's reading is "the shortest way
out", which in a planar crowd is always in-plane. Let it also consider up, and
a crowded region resolves by stacking rather than by spreading -- which is the
behaviour `Shape::Tall` was approximating by hand, arrived at by the physics
instead of by a hardcoded three.

The second is preferred because it makes height a consequence of crowding,
which is what it is: a circuit with room stays flat and pays nothing, and a
circuit without room grows the way redstone can. It also gives the seeded
perturbation nothing to do, so the seed goes back to being only a way to retry
a stuck configuration.

### Nothing holds a body up

§7.1 warned that legalising onto a 3D redstone lattice is far harder than onto
2D standard-cell rows, and this is where that bites. The design lets a body sit
at any height and never says what it stands on.

Dust needs a solid block beneath it. A repeater and a lever need one. A wall
torch needs a block on the face it attaches to -- which the `OnFace` weld
already covers, and it is the only one of the four that does. A support block
itself needs nothing, which is why nobody has noticed that the planner leaves
air under every one of them.

Today the question does not arise: everything sits at one Y, and what stands on
something stands on floor that emission laid or replayed without anyone
deciding it.

Once separation may push upwards it arises immediately. A repeater relaxed to
Y = 7 needs a solid block at Y = 6, that block occupies a cell, and that cell
participates in separation like anything else. Height is not free space; it is
space that has to be built up to.

Support is therefore a **body**, not an afterthought: every primitive that
needs one gets a companion at a fixed offset below it, welded there, with its
own extent in the separation. Which makes floors a placement decision rather
than something emission does silently, and makes the count honest -- a stacked
layout pays for its floors in the same units as everything else, so the
relaxation can see that stacking costs blocks and decide accordingly.

This is a real addition, not a clarification. It roughly doubles the body count
for a dust-heavy circuit, and it is the reason the estimate "relaxation is a
small module" should be distrusted until measured.

### Support blocks are not all alike

Supports being bodies raises a question the single-plane world never had to
answer: which of them conduct, for a separation that only applies between
different nets. It is not obvious, and it was got wrong in the code before it
was got wrong here.

A floor under dust is inert: another net may run beside it. A **NOR's support
is the gate's input node** -- dust laid against it powers it and turns the
torch off -- so it separates from foreign nets like any conductor. That was
found on 2026-08-12 by the planner placing a full adder that passed all four
invariants and computed the wrong sum, and it is fixed in
`gate_footprint` today.

So a support body carries the same port-level distinction as any other: a
NOR's support is a conductor on the gate's *input* net, a floor is a conductor
on nothing at all. Getting this wrong does not produce an illegal circuit. It
produces a legal one that computes something else, which is worse, and no
invariant catches it.

### Orientation has nowhere to go yet

The headline of this design is that torque decides facing. Walking stage 1
through to the blocks: it cannot, and the gap is wider than a missing
parameter.

`PlanCandidate` already carries `variant_indices`, one per body, which is
exactly where a chosen facing belongs. It is set to `vec![0; n]` at every
construction site and read by nothing. `place_nor_gate(world, origin,
input_count)` takes no facing, `gate_footprint(origin, gate)` takes none
either, and `emit_primitives` calls them without one. A relaxation that turns
every gate to face its consumer would hand that decision to a pipeline in which
every gate faces north, and the tests would pass, because nothing downstream
disagrees with a facing nobody applies.

So stage 1 owes three changes it would be easy to leave out:

- `place_nor_gate` and `place_merge_gate` take a facing, and place their
  support, torch and sockets rotated by it -- which is what
  `physical::variants` already describes and they currently ignore;
- `gate_footprint` takes the same facing, so the cells a body occupies and the
  cells it keeps others out of are the rotated ones;
- `emit_primitives` reads `variant_indices` instead of assuming zero.

And a fourth, which is not in emission at all. `route_every_net` finds a
socket with `step(support, INPUT_DIRECTIONS[input_index])`, and
`INPUT_DIRECTIONS` is `[West, East, South]` -- the sockets of a gate facing
north. Rotate the gate and its sockets rotate with it, so the router looks for
them in the wrong cells, and the approach cell it derives from socket and
support is wrong by the same rotation.

Counting where else that assumption lives, rather than assuming it is those
two: `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` are read in **five modules**.
`compile/mod.rs` places and emits with them. `planner.rs` routes and finds
output pins with them. `equivalence.rs` walks `INPUT_DIRECTIONS` to decide
which sockets actually feed a gate, and `OUTPUT_DIRECTION` to find its pin.
`world_partition.rs` resolves a node's position from an `INPUT_DIRECTIONS`
offset. `routing_stats.rs` finds a source pin with `OUTPUT_DIRECTION`. The last
two are what the viewer's topology view and several tests are built on.

So orientation is not four small changes. It is one assumption held in five
places, and rotating a gate falsifies all of them at once -- silently, because
each is a lookup that still returns a cell, just the wrong one.

Which argues for doing it as its own piece of work, before any relaxation
exists: make `physical::variants` the single place a gate's geometry is
written, and have all five ask it for the sockets and pin of a gate at a given
facing. That is a refactor with no behaviour change while every facing is
north, testable on its own, and it turns "orientation reaches the blocks" from
a cross-cutting change into a value that one function already returns.

Stage 1 should therefore be two: the geometry refactor, then the relaxation.
The estimate that called this "three changes, none large" was made by looking
at emission and not grepping.

### Where the relaxation starts

A spring system with hard constraints is not convex, so the starting point
decides which solution it finds. Starting everything at the origin gives a
knot the projection then has to unpick, and starting at random makes the result
unreproducible unless the randomness is the seed's.

It starts from the placement that exists today: Z by logic depth, X by
barycentre over already-placed sources. That layout is legal, it is what the
current `plan_from_netlist` produces, and it is measurably poor -- 572 blocks
for and4 against the emitter's 472, and 24 game ticks against 18. Relaxation is
therefore asked to improve a known-bad answer rather than to invent one, and
the improvement is measurable against the numbers it started from.

`RelaxEffort`'s seed perturbs that start. It exists so a run can be repeated
exactly and so a stuck configuration can be retried from a different one, not
because anything in the solve is random.

### What `physical.rs` has to gain

It gives four discrete facings for a torch, a repeater, a lever and a lamp,
each with typed ports and the blocks it occupies. Relaxation needs port offsets
at an *arbitrary* angle.

It gives none at all for a comparator: `variants(Primitive::Comparator)`
returns an empty slice, while `PortKind` declares `ComparatorRear`,
`ComparatorSide` and `ComparatorFront` that nothing constructs. No library
entry uses a comparator today, so relaxation never meets one -- but "never"
here is an accident of the current library rather than a rule, and a body whose
primitive has no variants must be an error naming the primitive, not a silent
placement of nothing.

The addition is small: treat facing 0 as canonical and rotate. The four
variants become the four quantised cases of one layout, so the continuous stage
uses the canonical version and `snap` looks the rotated result back up among the
four. The module stops being dead code and becomes the thing both stages share.

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

The mapping is not a coincidence, which an earlier draft of this section
claimed. `Provenance::Gate { gate, role }` is on every node, and
`PrimitiveGraph::gate_nodes[gate]` answers "which primitives are this gate" in
O(1). So `snap` groups its bodies by provenance and hands back one anchor per
gate, deliberately, and the grouping is already correct for a gate of five
primitives -- it just has nowhere to put the other four.

That is the whole of the seam: not the mapping, which exists, but
`PlanCandidate` having one anchor per gate to put the answer in. Design H's
five primitives would relax to five positions and be collapsed to one on the
way out.

Supports widen the same seam. `snap` returns anchors keyed by `NodeId`, and a
support body has no node -- so once floors are a placement decision rather than
something emission lays silently, the output has to carry them and
`PlanCandidate` has nowhere to put them.

Where floors come from today is worth stating exactly, because it is three
places and none of them is a decision. `emit_primitives` calls `ensure_floor`
once, for a gate's output pin. `emit_routes` writes floors it *replayed* from
what the legacy emitter recorded. And `place_nor_gate` lays none at all: a
gate's support sits on whatever happens to be beneath it, which in a
legacy-seeded world is floor the router laid for its own reasons.

Which means supports already float. Under the planner's own placement, the cell
below a NOR's support is air -- observed on 2026-08-12 while tracing a dead
signal, at `(14, 0, 5)` under `g0`. It does not matter yet, because a support
block needs nothing beneath it to work. It starts mattering the moment anything
is placed above one.

Which is why supports arrive in stage 2 and not stage 1. Stage 1 leaves every
body on the plane its starting layout gave it, floors stay emission's business,
and `snap` returning gate anchors is honest because nothing else exists to
return. Stage 2 pays for both at once: `PlanCandidate` carries primitives and
their supports, and `emit_primitives` stops inventing floors.

This document does not do that work. It names the seam, so that whoever widens
it -- for supports or for the DFF -- finds it described rather than discovers
it.

### Pinned ports and the shape preference

`PortPlacements` survives unchanged and gets simpler: a pinned port is a body
with infinite mass. It contributes force to its neighbours and takes none, and
`snap` returns it exactly where it was pinned.

`Shape::Tall` does not survive. It packs a level of logic wider than three
gates onto the storey above -- a hand-made rule standing in for what separation
now does when it is allowed to push upwards. Keeping both would mean two
mechanisms deciding height, one of them by a hardcoded three.

Its test changes rather than disappears. `a_tall_preference_uses_height_where_a
_wide_one_uses_floor` asks for `Shape::Tall` and checks the result is taller
and narrower; with no knob to ask, it becomes: six gates that all consume one
signal, crowded enough that spreading sideways costs more than stacking, end up
on more than one level. That is a weaker claim than the current test makes, and
it is the true one -- height is now earned by crowding rather than requested.

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

Each test names one claim, and the stage it belongs to -- the staging section
below splits this design into three, and a test for something a stage does not
build yet is a test nobody can write.

**Stage 1 -- relaxation and snapping, one plane**

1. **Determinism.** Same graph and effort, identical placement, bit for bit.
2. **Orientation.** A hand-built pair, stated as geometry rather than as a
   compass bearing, since "faces east" means different things for a wall torch
   and a repeater: a repeater whose only consumer sits to its east ends up
   driving its front eastwards, and a torch whose support sits to its west ends
   up attached to that face. This is the claim that torque produces
   orientation, on a case small enough to verify by hand.
3. **Separation after snap.** For every reference circuit, no two *ports*
   carrying different signals are closer than the rule allows -- ports, not
   bodies, because a gate is where one net ends and another begins, so its
   support and its torch are never on the same net. Checked directly on the
   snapped anchors, not through the invariants. This is what the rounding
   margin claims, and it is what an earlier draft got wrong by forgetting
   rotation, so the case that tests it is one that rotates: a body whose
   relaxed facing sits near 45 degrees, where quantising moves it furthest.
4. **Welds survive snap.** A torch is on the face of its support.
5. **Corridors exist.** A placement is not merely legal but routable: the
   router reaches every sink it could reach from the old placement. This is
   what the third separation term claims, and it is the one with no precedent
   to lean on -- legacy reserves routing space by construction and this does it
   by a number that was guessed. It says "could reach from the old placement"
   rather than "every sink" because `segment_a` and above do not route today
   whatever places them, and this test is about placement, not about fixing
   that.
6. **Better than what it replaced.** and4 placed by relaxation against and4
   placed by rows and barycentres: 572 blocks and 24 game ticks are the numbers
   to beat, and if neither is beaten the design failed at the thing it was
   written for. Both, because rows and barycentres are already smaller than
   they were and slower than the emitter -- beating one by giving up the other
   is not an improvement, it is a different trade.

**Stage 2 -- supports as bodies, separation that may push upwards**

7. **Every body stands on something.** After snap, each primitive that needs
   support has one, at the offset its weld requires.
8. **Crowding produces height.** Six gates all consuming one signal, packed
   tightly enough that spreading sideways costs more than stacking, end up on
   more than one level. This replaces the test `Shape::Tall` currently has, and
   claims less: height is earned rather than requested.
9. **Native and wasm agree.** The same circuit placed by both toolchains gives
   identical anchors. If it does not, positions become fixed-point; see above.
   Stage 2 rather than stage 1 because stage 1 does not reach `compile()`, so
   the viewer is still drawing the legacy placement and a divergence would
   mislead nobody yet.

**Stage 3 -- the switchover**

10. **End to end.** Every reference circuit places, routes, verifies, and
    matches its truth table.

**Whenever the DFF lands**

11. **The lock weld survives snap.** Design H's lock repeater is at the data
    repeater's side. Not written yet, and not writable: `compile()` rejects
    `GateKind::DffPosedge` before placement, so there is no Design H region to
    place. Listed because `BesideAt` exists in this design for it, and an
    unexercised constraint is one nobody has checked.

## The condition for switching `compile()`

All four hand-written circuits and both Verilog circuits must place, route,
verify, and match their truth tables. Not one fewer.

That condition may be unreachable for a reason this document does not own.
`segment_a` and `seven_segment` do not route today under the planner's own
router, whatever places them -- the failure recorded in
`how_far_the_planners_own_placement_carries` is the router running out of room
and rip-up failing to find any. A better placement may well fix it, since
crowding is what it ran out of. It may equally not.

If placement measurably improves and routing still fails at that size, the
answer is not to weaken this condition. It is that routing became the next
piece of work, and this design says so in advance rather than discovering it
and quietly shipping a `compile()` that only handles small circuits.

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

## Staging

Review grew this design twice: supports became bodies, and separation gained a
routing reservation. Both were necessary and both are load-bearing, and the
result is larger than one sitting's work. It is still one design -- every part
of it exists to make the same placement legal -- but it should land in three
pieces, each of which leaves the tree green:

0. **One place for a gate's geometry.** `physical::variants` becomes the only
   thing that knows where a gate's sockets and output pin are, and the five
   modules that hardcode `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` ask it
   instead, passing a facing. No behaviour changes while every facing is north,
   which is what makes it testable on its own -- every existing measurement
   must come out identical.

1. **Relaxation and snapping, primitives only.** With geometry already asking
   for a facing, a chosen one reaches the blocks by being passed rather than by
   changing five modules at once. No support bodies, no upward separation. Bodies stay at the Y their starting layout gave them, so nothing
   needs holding up and the third dimension is not yet in play. This is enough
   to answer the question the whole design exists for: does relaxation place
   better than rows and barycentres. `plan_from_netlist` switches to it;
   `compile()` does not.

   `Shape::Tall` survives this stage, which matters because otherwise the stage
   does not leave the tree green. It chooses the *starting* layout, and a stage
   that never moves a body in Y hands back whatever storeys it was given, so
   `a_tall_preference_uses_height_where_a_wide_one_uses_floor` still passes
   unchanged. It is stage 2 that takes the knob away, and stage 2 that replaces
   its test.
2. **Supports as bodies, and separation that may push upwards.** Height becomes
   available and starts paying for itself, and `Shape::Tall` is removed here
   because this is what replaces it: its test becomes "crowd it and it stacks"
   rather than "ask for tall and get tall".
3. **The switchover.** `compile()` moves once every reference circuit places,
   routes, verifies and matches its truth table.

Splitting it this way also gives the answer to "is relaxation better" before
the expensive part is built, which is the order this session's measurements
argue for.

## Out of scope

- **Routing.** The A* and rip-up path stays as it is -- with one exception this
  design forces and cannot avoid: the router derives a gate's sockets from a
  fixed `INPUT_DIRECTIONS`, so it has to be told the facing that relaxation
  chose. That is a substitution, not a change of algorithm, and everything else
  about routing stays out of scope: changing placement and routing together
  would leave nothing to attribute a regression to.
- **Weighting springs by criticality.** Timing-driven placement is the obvious
  next question and is deliberately deferred until a measurement says plain
  wirelength misses the 15-cell constraint often enough to matter.
- **The optimiser.** `optimise` keeps its move set. With placement decided by
  physics rather than by a grid, whether local search still has anything to
  contribute is a question for after this lands, and the measurement above says
  it currently does not.
