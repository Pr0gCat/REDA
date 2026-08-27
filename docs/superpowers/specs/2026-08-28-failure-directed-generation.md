# Failure-directed generation: form is a search variable

**Status: DELIVERED, same night.** Written the night of 2026-08-28 under a
standing goal: a generation method that takes a netlist in and hands a
simulator-verified redstone circuit out, with `segment_a` routing AND
verifying through the planner as the acceptance case, and only game mechanics
admitted as rules.

**The result.** `compile_grown` -- place, negotiate (`Wide`, never lays a
ring), and when routing fails, multiply the hottest quarter of bodies'
separations by 1.4 and place again (`GROWN_SHIPPING_RULE`). On segment_a,
the full battery through the real Simulator
(`measure_the_production_growth_loop`):

    ROUTED and VERIFIES in 254.5s | 47 routes | verify: ok
    0 ring(s); 0 latched cell(s) with every source deleted
    truth table: 16/16 | worst settle: 68 game ticks
    5,874 blocks, box 114x5x117   (legacy ships 6,416 / 68 ticks / 137x6x182)

The first segment_a this tree ever produced outside the legacy emitter --
and it is smaller than the legacy one at the same worst settle. The winning
mechanism is the complement the night's probes measured: rip-up lays latches
at every density (inflation alone: laid 28 -> 43 of 47, never routed);
negotiated-Wide never lays a ring and only ever starved for room; room
where the heat is plus Wide carries it end to end. The repair-phase design
below (§ The design) was measured insufficient on its own and stands as the
record of why the placer side had to move.

## The diagnosis this rests on (measured)

The compiler's search space had exactly one degree of realisation freedom:
**where** a gate stands. What a gate *is* -- its facing, its footprint, which
cell its sockets open onto -- was decided by `snap` before routing said a
word, and no routing failure could reach back into any of it. Every search
this branch tried (rip-up ordering, negotiated pricing, congestion history,
growth, SAT, a spring rest length) moved prices or positions inside a form
set of size one per gate.

Tonight's probes measured what that costs, and what each freed variable buys:

| probe | result |
|---|---|
| weld-sibling exemption | **shipped.** The projection's separation between two repeaters welded into one junction's sockets was our rule, not the game's. Exempting weld-determined siblings un-deadlocks every double-socket circuit: verilog:seven_segment places (both lowerings, 10 steps), the 5-gate minimal netlist compiles clean through `compile_planned`. |
| reface (rip-up) | 0/18 single turns route segment_a -- but 15 of the 18 failures are the ring rule or decay, not doorways. The rip-up router searches with `OwnJoinPolicy::Off`: it proposes latch corridors and burns its 64 rounds re-proposing them. Facing *is* live (turning g23/g20 east moves laid 28 → 36); under `Off` it cannot matter, because no facing fixes a latch. |
| already-fed (negotiated) | The freeze (contested 0 / unlaid 6 from iteration 10) reproduces exactly. The six dead branches' sockets sit chebyshev 2..14 from their own trees; none is already joined; none is a same-gate multi-input case. "Wide walls a branch off from a socket its own wire already feeds" is **refuted** for segment_a. |
| branch order (global) | Farthest-first turns the freeze into an oscillation: different nets die, and the failure kind shifts from doorways to decay-with-no-refresh (branches now ride long trunks and hop off tired). Order changes *which* nets die -- it is a real knob and no single setting wins. |
| branch order (per-net, cycled on failure) | No convergence in 32 iterations. The same six high-fanout nets (g0,g1,g2,g4,g5,g6) rotate in and out of death. Diagnosis: price convergence wants stillness and knob-cycling injects churn -- two dynamics fighting in one loop. |

## The design (being built)

**One loop, two phases, and the failure kind picks the knob.**

Phase 1 — *settle*: negotiation exactly as shipped (`Wide`, the present
schedule, history charging). Run until the contention trace stabilises: the
measured shape is contested → 0 while a small tail of nets stays unlaid for
non-contention reasons.

Phase 2 — *repair*: the tail is not a pricing problem (measured: its cells
have no alternative at any price, and its failures charge nothing). It is a
small discrete CSP over realisation choices, and the failure that named each
dead net also names its knob:

- `NoLocalRoute` (a doorway): try that net's branch orders, then the sink
  gate's other three facings. A socket's only approach cell is a function of
  facing, so turning the gate is the one move that relocates a door.
- decay-with-no-refresh: the branch rode a trunk and hopped off tired. Knobs,
  in order: a branch order that lays this sink earlier (independent corridor,
  full strength), then trunk retrofit -- upgrading a straight own-trunk dust
  cell to a repeater before the hop-off, which the game allows and the
  one-shot branch realisation currently cannot express.
- ring (should not appear under Wide; counted if it does): the search-time
  tree rule leaked -- record the corridor, refuse the join cell, re-lay the
  net.

Repair choices are pinned per net once they lay, the loop returns to phase 1
to let prices absorb the new trees, and the exit stays exactly as shipped:
every net laid and nothing contested in the same iteration, then
`negotiation_left_nothing_shared`, then the four invariants and the truth
table. **Routing is never the acceptance condition** -- two prototypes on
this branch turned route failures into signal-strength violations, so a
candidate that routes and does not verify has bought nothing.

In parallel, the placer's side of the bargain: the congestion-driven
inflation loop (trial-route → charge → inflate the hot bodies' separations →
re-place) is being measured tonight as the way to buy the maze more room
*where routing said it needs it*, not uniformly -- uniform room is already
refuted twice (the 2x-anchor scaling and the rest-length sweep).

## What this deletes if it works

The hybrid `compile()`'s legacy fallback for segment_a, and eventually the
legacy emitter for every circuit the planner can carry. The pinned block
counts (232 / 1,065 / 6,416 / 16,244) move only when a circuit deliberately
changes producers.

## What is deliberately out

- A shape library (second footprints per primitive). Facing is the whole
  form budget until the four facings are measurably exhausted.
- Analogue strength signalling. Scheduled to break `MAX_DUST_RUN` reasoning
  later; not tonight.
- Any knob turned without a failure naming it first.
