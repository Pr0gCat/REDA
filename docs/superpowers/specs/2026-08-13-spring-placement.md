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
not a rigid body. (Review found one mover that is not a primitive: a wire
merge, which contributes no primitive at all -- see "A third of the gates have
no body". It also spent three rounds inventing support bodies before reading
that a variant's blocks already include what the component stands on -- see "A
body already carries what it stands on".) The cost is that the one geometric
relation between *different* bodies -- Design H's lock repeater at the data
repeater's side -- has to become an explicit constraint. The gain is that
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
its degree less the edges that stay inside one body, because a torch's support
is one of the torch's own cells and no wire runs between them -- times the
width one route needs.

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
  |  snap                         NEW -- round positions to the lattice
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
    /// One of four. Never continuous: a body's best facing is chosen by trying
    /// all four against the pulls on its ports, so there is no angle to
    /// integrate and none to quantise later. Meaningless for a junction --
    /// dust is isotropic, so a merge has no front to turn.
    facing: Facing,
    // No stored extent. With a facing always one of four, the cells a body
    // occupies are `variants(kind)[facing].blocks` -- already axis-aligned,
    // already exact. An earlier draft carried half-extents because it expected
    // to need them at arbitrary angles, and a radius before that, which would
    // have wasted the space this design exists to win: a NOR cell is not a
    // sphere.
}

/// A spring, attached at a port on each end.
struct Pull { from: (usize, Attach), to: (usize, Attach), stiffness: f64 }

/// Where on a body a spring attaches.
///
/// `PortKind` names the ports of the four components that have an
/// orientation, and nothing else: there is no variant that can name a wire
/// merge's socket or its outbound pin, because `physical.rs` has never modelled
/// a merge. A junction's attachments are given by index instead -- input `i`,
/// or the outbound pin -- which is all a merge's geometry distinguishes.
enum Attach {
    Port(PortKind),
    JunctionInput(usize),
    JunctionOutput,
}

enum BodyKind {
    /// A component the primitive graph named. Its blocks include whatever it
    /// stands on or attaches to; `physical.rs` has always said so.
    Primitive { node: NodeId, kind: Primitive },
    /// A declared wire merge. It has no primitive and no facing -- see
    /// "A third of the gates have no body".
    Junction { gate: usize },
}

/// A relation between two bodies that must hold exactly. Projected, never
/// pulled.
///
/// One variant, because the others turned out to be relations inside a single
/// body: a torch and its support, a repeater and its floor, are one body each.
enum Weld {
    /// Design H's lock repeater at the data repeater's side.
    BesideAt { lock: usize, data: usize, side: RelativeSide },
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

A step is not a gradient step. The objective is quadratic, so with the
constraints held aside it has a direct solution -- the linear system
`Ax = f + c` that the founding spec records LogicLoom solving -- and a step is:
solve it exactly for the current facings, then project. Repeat.

That matters because the alternative needs a step size, and a step size is a
constant with no derivation, which is the third one this design would have
carried if nobody looked. Solving exactly has none: the springs decide where
the bodies want to be, the projection decides where they may be, and neither
asks how far to move.

Holding facings aside is what makes it linear, and raises the question the
design had left out entirely: how a facing changes at all. Torque was named as
the thing that produces orientation and never given a rule.

It gets one, and the same trick applies. With positions held, each body's best
facing is a one-dimensional question with four answers -- the pulls on its
ports are known, and the facing that minimises their energy is found by trying
all four and keeping the least. Not a rotation integrated over time: an
enumeration, because there are four.

So a step is three things, alternating: solve positions exactly for the current
facings, choose each body's best facing for the current positions, project.
Block coordinate descent, with the block that would have been hard reduced to a
choice among four. It also means facings are never continuous during the solve,
which retires `snap`'s first step -- there is nothing left to quantise -- and
leaves `snap` as re-project and round.

`Body::facing` is therefore one of four, not radians. The type said `f64`
because the design assumed torque had to be integrated; it does not.

Convergence is reached when no body moves more than a tenth of a cell in a
step. A tenth because the rounding margin is a whole cell, so a system still
twitching below that cannot change what `snap` produces; running past it buys
nothing measurable. Reaching `RelaxEffort::iterations` without reaching that
threshold is `DidNotConverge`.

### The separation the projection enforces

Four terms, each with a source:

1. the cells each body occupies, which `variants(kind)[facing].blocks` gives
   exactly;
2. two cells of conductor clearance, from
   `2026-08-09-channel-safety-condition.md`, between different signals only;
3. room for the nets that must reach the body -- the edges that need routing,
   which is its degree less its welds, times one route's width, so that a
   relaxed placement has corridors rather than only clearance;
4. one cell of rounding margin, which is what rounding a position can cost --
   see "What `snap` has left to do".

### How welds and separations compose

The projection has two kinds of constraint and the design has not said how they
compose. A weld is an equality -- Design H's lock repeater sits exactly beside
its data repeater, no nearer and no further. A separation is an inequality --
at least this far from a foreign conductor. Pushing the lock away from a
foreign net drags it off the data repeater's side unless the weld is restored,
and restoring the weld can push it back into what the separation just cleared.

They compose by alternating, welds last: separate every violating pair, then
re-satisfy every weld, and repeat until neither moves anything. Welds last
because a weld violated is a circuit that does not work, while a separation
violated is a circuit that works and is illegal; if only one can hold at the
end of a step, it must be the one whose failure the invariants would not catch
as a wrong answer. Not converging is the deadlock the error table already
names, and it is a real outcome: three bodies that must each touch a fourth and
each stay clear of the others may have no arrangement at all.

A weld's offset is a function of facing. The lock sits at the data repeater's
*side*, and which cell is "the side" turns with the data repeater -- which is
why a step re-satisfies welds after choosing facings and not before: a body
that turned has moved the cell its weld points at, and the weld has to be
restored at the facing that will actually be built.

With one weld in the design and the DFF that needs it not yet compilable, this
machinery is exercised by nothing until the DFF lands. It stays, because the
alternative is the DFF arriving to find welds unspecified -- but a reader
should know that everything in this section is, today, a contract with no
caller.

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
circuit without room grows the way redstone can. It also leaves the seed with
one job instead of two -- retrying a stuck configuration, not breaking
symmetry.

### A third of the gates have no body

"Bodies are primitives" excludes more than it looks. A wire merge contributes
**no primitive at all**: `topology`'s own test says so by name --
`or_bare_entry_has_no_nodes_no_inputs_and_no_output_primitive`, "a bare merge
places nothing" -- with an empty template, no internal edges, and no node for a
declared input to land on. `primitive_graph::expand` therefore produces nothing
for it.

More precisely, because the two kinds of merge differ and the difference
matters. For a **bare** branch, `expand` does `contributions.extend(producer)`:
the merge is transparent, and its consumers are wired straight to its
producers. For an **isolated** one, each input gets an `IsolatingRepeater`,
which is a real primitive with a real node.

So an isolated merge is placed by its repeaters, and a bare merge is placed by
nothing at all -- while `place_merge_gate` writes blocks at its anchor either
way, and routes terminate on its sockets. In `verilog:seven_segment` that is 17
merges of 47 gates.

Bodies therefore come from the primitive graph's nodes **and** the netlist's
merges. A merge's body is a junction: extent from what `place_merge_gate`
occupies, ports at its input sockets and its outbound pin, and no facing at
all. Dust is isotropic -- it has no front -- so a junction is the one body for
which torque means nothing.

Note what this does to the spring network. The graph wires a bare merge's
consumers directly to its producers, so springs alone would pull those two
groups together and never notice the junction sitting between them. Adding the
junction as a body means also re-inserting it into the pulls: producer to
junction, junction to consumer, in place of the through-edge the graph
provides. Placement's graph is not quite the primitive graph, and that is the
first place they part.

That this had to be found rather than being obvious is the cost of describing
placement in terms of `physical.rs`, which models the four components that have
an orientation and has never had anything to say about the one that does not.

### A body already carries what it stands on

§7.1 warned that legalising onto a 3D redstone lattice is far harder than onto
2D standard-cell rows, and three rounds of this review went looking for that
difficulty in the wrong place: they concluded that a body at height needs a
support block placed under it, made supports bodies, welded them, and worked
out which of them conduct.

`physical.rs` had already answered it. Every variant includes the block its
component stands on or attaches to:

- a torch is `{ORIGIN: Solid, NORTH: WallTorch}` -- the support is the torch's
  own block, and a support block needs nothing beneath it, which is why the
  cell under one is air today and always has been;
- a repeater is `{DOWN: Solid, ORIGIN: Repeater}`;
- a lever is `{DOWN: Solid, ORIGIN: Lever}`.

So there is no support body, no `StandsOn`, and no `OnFace`: a torch and its
support are one body, not two welded together. What holds a body up is part of
what a body is, and it separates from foreign nets like the rest of it, by the
term that already says "the cells each body occupies".

What does still need floors is dust the *router* lays, which routing already
handles by recording the floor under every cell it writes. That is out of scope
here, and it was the whole of the real problem.

The lesson is narrower than "check physical.rs". Three rounds reasoned forward
from "bodies are primitives" to what a primitive must therefore need, and never
read what the module that describes a primitive already said it has.

### A NOR's support is a conductor, and it is one of the torch's own cells

Which cells of a body conduct is not uniform, and the case that matters was
found the expensive way. A **NOR's support is the gate's input node**: dust laid
against it powers it and turns the torch off. On 2026-08-12 the planner placed
a full adder that passed all four invariants and computed the wrong sum,
because a foreign net was free to run against a support the code treated as
inert. `gate_footprint` marks it a conductor today.

Since a torch and its support are one body, that is a statement about a body's
cells rather than about a separate support body: the `ORIGIN` cell of a torch
variant carries the gate's input signal, its `NORTH` cell carries the output,
and the separation is between cells carrying different signals. A repeater's
`DOWN` floor carries nothing and is inert.

Getting this wrong does not produce an illegal circuit. It produces a legal one
that computes something else, which is worse, and no invariant catches it.

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

`RelaxEffort`'s seed perturbs that start, and that is its only job: a stuck
configuration can be retried from a slightly different one, reproducibly.
Nothing in the solve is random, and the seed is *not* what breaks the planar
symmetry -- upward separation does that, for the reason given above. An earlier
draft had it doing both, which would have made every flat circuit slightly
wrinkled for no benefit.

### What `physical.rs` has to gain

Nothing, which is not what this section said until facings stopped being
continuous.

It gives four discrete facings for a torch, a repeater, a lever and a lamp,
each with typed ports and the blocks it occupies -- and four discrete facings
is exactly what the solver chooses among. The rotation machinery an earlier
draft asked for, a canonical layout rotated to arbitrary angles, has nothing to
do: the enumeration reads `variants(kind)[facing]` and takes the ports as
given.

It gives none at all for a comparator: `variants(Primitive::Comparator)`
returns an empty slice, while `PortKind` declares `ComparatorRear`,
`ComparatorSide` and `ComparatorFront` that nothing constructs. No library
entry uses a comparator today, so relaxation never meets one -- but "never"
here is an accident of the current library rather than a rule, and a body whose
primitive has no variants must be an error naming the primitive, not a silent
placement of nothing.

And it gives nothing for a wire merge, which is why a junction's attachments
are indices rather than `PortKind`s. See "A third of the gates have no body".

### What `snap` has left to do

An earlier draft made `snap` three steps, the first of which quantised a
continuous facing, and argued at length that the rounding margin had to cover
the ports swinging up to 45 degrees as it did. Facings are never continuous, so
none of that survives: the solver has already chosen one of four, and snapping
has no angle to resolve.

What is left is rounding positions, and one cell of margin covers it by the
argument that was always true for translation alone -- rounding moves a body by
at most half a cell, so two bodies approach by at most one, and a continuous
solution separated by the requirement plus one is still separated after.

`snap` still returns a `Result`, for a smaller reason: rounding is exact only
if the projection converged, and a placement handed to `snap` unconverged has
no margin to spend. Rounding it anyway would produce the class of failure this
design exists to avoid -- a layout that looks placed and is illegal in ways the
invariants find later and attribute elsewhere.

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
Nothing here orders anything: the relaxation solves, `snap` rounds, and what
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
   margin claims, and the case that tests it is one where a body sits close to
   a half-cell boundary in every axis at once, which is where rounding moves it
   furthest.
4. **Welds win.** A body forced by separation away from something it is welded
   to ends the step welded, and the separation is the constraint left violated.
   It is the order the projection promises, and the one whose reverse would
   produce a circuit that does not work. With `BesideAt` the only weld and the
   DFF not yet compilable, this is tested on a synthetic pair of bodies rather
   than a circuit -- which is honest, because the contract is the projection's,
   not the DFF's.
5. **An unconverged placement is refused, not rounded.** `snap` on a placement
   whose projection ran out of iterations returns an error naming the worst
   remaining violation. Rounding it would spend a margin that is not there, and
   the resulting illegality would surface later attributed to something else.
6. **Corridors exist.** A placement is not merely legal but routable: the
   router reaches every sink it could reach from the old placement. This is
   what the third separation term claims, and it is the one with no precedent
   to lean on -- legacy reserves routing space by construction and this does it
   by a number that was guessed. It says "could reach from the old placement"
   rather than "every sink" because `segment_a` and above do not route today
   whatever places them, and this test is about placement, not about fixing
   that.
7. **Better than what it replaced.** and4 placed by relaxation against and4
   placed by rows and barycentres: 572 blocks and 24 game ticks are the numbers
   to beat, and if neither is beaten the design failed at the thing it was
   written for. Both, because rows and barycentres are already smaller than
   they were and slower than the emitter -- beating one by giving up the other
   is not an improvement, it is a different trade.

**Stage 2 -- supports as bodies, separation that may push upwards**

8. **Every body stands on something.** After snap, each primitive that needs
   support has one -- which is a test that its own blocks were written, not
   that a separate body was placed under it.
9. **Crowding produces height.** Six gates all consuming one signal, packed
   tightly enough that spreading sideways costs more than stacking, end up on
   more than one level. This replaces the test `Shape::Tall` currently has, and
   claims less: height is earned rather than requested.
10. **Native and wasm agree.** The same circuit placed by both toolchains gives
   identical anchors. If it does not, positions become fixed-point; see above.
   Stage 2 rather than stage 1 because stage 1 does not reach `compile()`, so
   the viewer is still drawing the legacy placement and a divergence would
   mislead nobody yet.

**Stage 3 -- the switchover**

11. **End to end.** Every reference circuit places, routes, verifies, and
    matches its truth table.

**Whenever the DFF lands**

12. **The lock weld survives snap.** Design H's lock repeater is at the data
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

0. **One place for a gate's geometry.** The socket and pin arithmetic takes a
   facing and answers with cells rotated by it, and the five modules that
   hardcode `INPUT_DIRECTIONS` and `OUTPUT_DIRECTION` ask it instead of
   assuming north. No behaviour changes while every facing is north, which is
   what makes it testable on its own -- every existing measurement must come
   out identical.

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
