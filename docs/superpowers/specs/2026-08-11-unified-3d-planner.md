# Unified 3D planner

## Status and decision

This document supersedes the planner portion of
`2026-08-08-primitive-level-flow.md`.  That document correctly identified
that rows, channels and tracks are the wrong *model*, but it still described
dust and support blocks as primitive-graph vertices.  The current topology
contract is narrower and correct: the graph describes **signal-bearing
primitives and their directed signal flow**.  Dust, support blocks, faces,
orientation, repeater refreshes and keep-out blocks are physical realisation
choices made by the planner.

The compiler must no longer treat placement and routing as two algorithms.
They are two fields of one candidate: changing a primitive's location implies
rerouting every incident signal, and a route collision can justify moving its
endpoint.  A candidate is accepted only when both fields form one legal
Minecraft layout.

## Input and output

```
gate-level netlist
  -> choose a topology-library entry per cell
  -> expand to a PrimitiveGraph (Torch / Repeater / Comparator / Lever / Lamp)
  -> optimise a single PlanCandidate
  -> realise it as blocks and write a World / Litematic
```

`PrimitiveGraph` remains coordinate-free.  It retains provenance so the
planner can report a cost for each originating cell and retry another
topology entry.  No pass attempts to recover an AND, MUX or OR from a lowered
NOR cluster: that information belongs to the gate-level netlist and must stay
there.

## Candidate model

`PlanCandidate` is the planner's single source of truth.  It contains:

- an integer `Anchor` for every signal primitive;
- a selected, orientation-specific **physical realisation** for each anchor;
- one directed `Route` for every `PrimitiveGraph::Edge`, expressed as a
  sequence of lattice waypoints and local terminal style;
- a derived occupancy/reservation map; it is never independently edited.

A physical realisation maps a signal primitive to a small set of blocks and
typed ports.  For example, a torch has input ports on its conductive support
and an output port at its torch; a repeater has rear/side/front ports; a lamp
has a powered input.  This is deliberately separate from gate topology: it
contains positions only *relative to an anchor*, and the global coordinates
belong exclusively to the candidate.

Routes own dust, terrain/riser blocks and routing repeaters.  Their terminal
is chosen by physical legality, not by a hard-coded gate path:

- `DirectedDustIntoSupport` is allowed only when the final straight dust run
  weak-powers the intended support from its direction and no side/corner
  attachment destroys that directionality.
- `RepeaterIntoSupport` is used when a diode is needed for isolation or a
  directed dust terminal is illegal.
- comparator and repeater ports use their actual rear/side rules, including
  the 26.2-confirmed weakly-powered conductive rear block behaviour.

The current row/channel/track router is retained only to create a first legal
candidate while the new planner is introduced.  It is not a permanent stage
of the final flow.

## Legalisation is part of every move

The relaxed optimiser may keep a continuous preferred position internally,
but it is never a result.  Every scored candidate is legalised before scoring:

1. snap or move anchors to the integer lattice;
2. place each local physical realisation and reserve its blocks and ports;
3. rip up and reroute only edges incident to moved anchors, using the current
   reservation map;
4. insert repeater refreshes when strength requires them;
5. reject the candidate if it cannot satisfy spacing, directionality,
   strength or port constraints.

This makes a move such as "move one torch toward its consumer" a joint
placement-and-routing operation rather than a placement change followed by a
separate global router.

## Objective, constraints and effort

The four existing correctness checks remain hard constraints: spacing,
connectivity, torch/merge semantics and signal strength.  A false circuit is
not an expensive circuit.

For a legal candidate, use a normalised, weighted score:

```
score = w_delay * critical_path_delay / seed_critical_path_delay
      + w_wire  * routed_lattice_length / seed_routed_lattice_length
      + w_space * occupied_bounding_volume / seed_occupied_bounding_volume
      + w_turn  * nonterminal_turns / seed_nonterminal_turns
```

`critical_path_delay` includes measured primitive delay plus placed routing
repeaters; it is the primary term.  `wire`, `space` and `turn` are physical
costs that can be updated from routes touched by a move.  Fill ratio and
air-below-block remain diagnostics, not terms: both have misleading degenerate
minima.

Weights are named fields in `PlannerWeights`, with all terms normalised to the
legacy seed.  `PlannerEffort` is a deterministic count of candidate
evaluations plus an optional stable seed.  The optimiser always retains the
best legal candidate, so stopping at any effort produces a legal circuit.
The default weights are chosen by a documented sweep over the reference
circuits, not by raw unit magnitudes.

## Optimisation loop

1. Expand the selected gate-topology entries and seed a legal candidate from
   the legacy compiler.
2. Run deterministic local moves in stable node-id order: anchor displacement,
   orientation change, terminal change, and route rip-up/reconnect.  Score the
   whole resulting legal candidate incrementally.
3. After an effort epoch, aggregate each gate's realised local cost from its
   primitives, ports and incident routes.  Offer the library's alternative
   entries only where that measured cost is lower, re-expand that gate region,
   and continue from the best legal candidate.
4. Stop at the effort budget, emitting the best candidate.  No accepted move
   may worsen the weighted score; ties use a stable structural ordering.

The first implementation may ship with only orientation and terminal choices
because most library entries currently have one implementation.  The loop and
per-cell cost report must exist before more topology techniques are added;
otherwise every new technique becomes another router special case.

## Evidence required before replacing the legacy emitter

- Candidate scoring is deterministic for fixed netlist, seed, weights and
  effort.
- A seed extracted from the existing emitted world is legal and has score
  exactly 1.0 for every nonzero normalised term.
- Moving an anchor reroutes only its incident edges; untouched route ids and
  score contributions remain unchanged.
- A directed-dust terminal is selected only for a shape that passes the
  directionality checker, with a repeater fallback for the adjacent/corner
  counterexample.
- Every emitted candidate passes all four physical invariants and existing
  truth-table tests.
- The five reference circuits do not regress against their then-current
  baseline.  The 26.2 client, not RCON `/setblock`, certifies dynamic diode
  and sequential behaviour.

## Out of scope

- The editor and its UI controls.
- Treating the 1.20.1 server as a product target; it is historical evidence
  only.
- Claiming a mathematically global optimum.  The implementation is a
  deterministic anytime optimiser over a stated effort budget.
