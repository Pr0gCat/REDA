# Routing at Scale

**Status:** design, not implemented. This is the piece of work
`2026-08-13-spring-placement.md` named in advance:

> If placement measurably improves and routing still fails at that size, the
> answer is not to weaken this condition. It is that routing became the next
> piece of work.

Placement measurably improved (anchor bounding boxes 2.6x–4.0x smaller;
and4 572 → 232 blocks, worst settle 26 → 14). Routing still fails at
`segment_a`'s size. So this document exists, and `compile()` did not switch —
see `.superpowers/sdd/task-13-report.md`.

Everything below is grounded in measurements taken between 2026-08-14 and
2026-08-15 at HEAD `0ac2688`. Where a claim is inference rather than
measurement it says so in those words. Where the two independent
investigations disagree, both are recorded and neither is picked.

---

## 1. What fails

Reproduced for this document, `cargo test --release --lib
compile::planner::tests::the_six_condition_circuits_stage_by_stage --
--ignored --nocapture`, whole run 55.84s:

| circuit | bodies | place | route |
|---|---|---|---|
| and4 | 11 | Ok 0.0s | Ok 0.0s |
| full_adder | 25 | Ok 0.0s | Ok 0.3s |
| segment_a | 50 | Ok 0.3s | **ERR 31.7s** — `no safe local route from (99, 1, 97) to (122, 1, 89)` |
| seven_segment | 88 | Ok 1.1s | **ERR 21.1s** — `no safe local route from (83, 1, 106) to (83, 1, 96)` |
| verilog:and4 | 13 | Ok 0.0s | Ok 0.0s |
| verilog:seven_segment | 74 | **ERR 0.9s** (placement — see §7) | NOT REACHED |

`seven_segment` has 84 gates to `verilog:seven_segment`'s 47 and places fine,
so §7's failure is not a size wall and this section's is not about gate count
either: `full_adder` at 25 bodies routes and `segment_a` at 50 does not.

### 1.1 Both printed addresses are artefacts. Do not scope work off them.

`route_every_net` returns `last` (`src/compile/planner.rs:2402`) — the error
from the final rip-up round. What a human reads is therefore whichever net
happened to fail on round 63, not the net that keeps failing. **Verified in
source for this document**, and both investigations measured the consequence
independently:

- `seven_segment`: `(83, 1, 106) → (83, 1, 96)` is net `g0` on round 63. The
  failure that *recurs* is `g16`, `(114, 1, 69) → (110, 1, 59)`, at rounds 0,
  2, 8, 10, 21, 40, 47, 52 and 60, with `g10`, `g19` and `g13` in the same
  region.
- `segment_a`: the printed net is `g32`. Over a 256-round run the blocked-net
  tally is `{g2: 59, g4: 53, g0: 43, g1: 22, d2: 13, g23: 11, g17: 7, g32: 7,
  …}` — 22 distinct nets take turns. `g32` is 7 of 256.

The address is *stable* run to run (measured: identical across every baseline
run, and identical to what `planner.rs`'s own doc records). Stability is
determinism, not representativeness.

**Consequence for the next task:** one of the two segment_a root-cause
dissections below was performed on `g32`, which its own author's tally shows
is 7 of 256. That is not a reason to discard it — the mechanism it found is
general — but it *is* a reason not to treat "unblock `g32`" as the goal.

---

## 2. The router, as it is today

All line numbers verified against `src/compile/planner.rs` at `0ac2688` while
writing this document.

**A laid net consumes a three-cell-wide corridor.** `anchor_is_free_for`
(1611) refuses a candidate cell on three independent grounds:

1. another net owns the cell (1619–1626);
2. the cell one level below holds *any* conductor, including this net's own
   (1638) — realisation writes a stone floor under every cell, and laying one
   over live dust deletes it;
3. any of the twelve cells `keep_out` (1659) returns — the four horizontal
   neighbours at `y-1`, `y` and `y+1` — holds a **foreign** conductor
   (1642–1649).

Rule 3 is the conservative plan-time shape of `connectivity::dust_reach`
(`src/redstone/simulator/connectivity.rs:133`), which needs a world that does
not exist yet when a plan is checked.

**The search is a plain A\* with a hard obstacle test.** `deterministic_astar`
(1329) bounds the box at `manhattan + 2` horizontally (1337) and
`start.y.max(goal.y) + CLIMB` above with `CLIMB = 3` (1346, 1354), floored at
`start.y.min(goal.y)` (1349) because the gate plane already stands on the
lowest floor there is. It seeds the frontier with **`start` alone** (1357) and
keeps one global `previous` parent map (1363).

**Multi-sink nets are routed as a star, not a tree.** Every branch calls
`deterministic_astar(source, approach, …)` from the same source cell; sharing
happens only when a later path happens to begin with an already-laid prefix
(`take_while(|a| route.anchors.contains(a))`). The net is all-or-nothing: if
the eighth branch fails, the seven already laid are discarded.

**The outer loop is not a negotiator.** `route_every_net` (2361) runs
`RIP_UP_ROUNDS = 64` (2359) rounds. Each failed round promotes the blocked net
to the front of the order and calls `Congestion::charge` (1558), which charges
every *foreign-owned* cell inside the failed corridor's bounding box — cells
that were never in the way included. `Congestion::price` (1542) is
`charged × 6`, flat, and **never decays**. `Reservation::cells_within` (1847)
computes `lo.y` and `hi.y` and then filters on **x and z only**, so a charge
covers the whole column at every height.

Negotiation, in the sense the literature means, requires that a blocked net be
able to *use* a contested cell at a price and be billed for it afterwards.
Here `anchor_is_free_for` refuses outright, so a blocked net cannot express
which cell it needed; the bounding-box charge is the only signal, and it is
indiscriminate.

---

## 3. Two independent diagnoses

Two investigations ran without knowledge of each other — call them **S** (took
`segment_a`) and **V** (took `seven_segment`). They agree on more than they
disagree, and the disagreement is worth more than the agreement.

### 3.1 What both measured, agreeing

- **The search is not giving up.** S: for `g32` the A\* frontier *empties* —
  `reached == popped == 256 cells`, closest 24 from a goal 31 away — rather
  than hitting a bound. V: `climb = 8` grew `g16`'s search from 4,873 to
  12,828 settled cells and still found nothing, closest to goal still 2;
  `margin + 40` grew it to 52,189 and still found nothing.
- **Widening the search does not help.** Both measured it. S additionally
  validated the variants that *did* report FOUND against the unrelaxed rules
  cell by cell: `no bounds` goes underground (`y = -1` for nine cells, 11
  faults), `CLIMB 3→9` flies at `y = 5..6` (9 faults), `no keep-out` has 15
  keep-out faults. All three are illegal paths, not missed legal ones.
- **More rounds do not help.** S: 256 rounds, 498s, `stopped_early = false`,
  deepest round laid 36 of 47 nets. Rounds also slow from 0.3s to 2–6s as
  ~5,000 cells accumulate charges that never decay.
- **Ordering alone does not help.** V: 120 seeded shuffles, one pass —
  `full_adder` min 0 / median 4 / max 6 refusals with **1 of 120 orders
  clean**; `segment_a` min 14 / p25 18 / median 20 / p75 22 / max 28 with
  **0 of 120 clean**. S: reordering branches *within* multi-sink nets,
  nearest-first and farthest-first, leaves both circuits failing.
- **`keep_out` against other nets' conductors is the dominant wall.** S's wall
  tally over `g32`'s pocket boundary: keep-out 963, bounds/stairs/self 494,
  floor-over-conductor 104, owner 77. V's refusal histogram for `g16` round 0:
  keep_out 3,524, floor-below 1,011, self_obstructs 1,884, owned 107,
  staircase 57, out-of-bounds 9,861.
- **The plane is far more sterile than it is occupied.** V, at
  `seven_segment`'s final failure search box (875 cells at `y = 1`): 187
  reserved, 688 unreserved, **431 refused to a stranger** — half the box
  unusable while 21% is occupied. S, at the corridor box (216 cells): 35
  reserved (16.2%), 23 conducting, and impassable.
- **The geometry admits the routes individually.** S: against primitives plus
  all 83 pre-claimed approach cells, "83/83 branches have a path; unreachable
  []". V: 0 branches unreachable at every iteration of the PathFinder probe on
  all three circuits, including the 23 `segment_a` and 55 `seven_segment`
  branches the shipping router refuses.

### 3.2 Where they disagree

**Disagreement 1 — is the placement implicated at all?**

| | claim | measurement behind it |
|---|---|---|
| **S** | The relaxed placement is the input that breaks a router that legacy's placement does not break. | Built a fresh `PlanCandidate` from `seed_from_legacy`'s anchors and fed it to the same 64-round loop: **`[legacy] ROUTED at round 25 after 104.8s`**. Relaxed never routes. Anchor box 89×91 = 8,099 relaxed vs 129×180 = 23,220 legacy; nearest-neighbour anchor distance median 7 vs 13. |
| **V** | "The router refuses; the placement is innocent. It has never routed any circuit larger than `full_adder` on **any** layout — including the legacy emitter's own." | One-pass census (no rip-up), `segment_a`: relaxed 60 routed / 23 refused, legacy anchors 60 routed / **23 refused** — identical. `seven_segment`: relaxed 101/55, legacy 121/35. Legacy anchor box 213×249 = 53,037 vs relaxed 184×93 = 17,112. |

**These measurements do not contradict each other; the conclusions do.** V ran
one pass with rip-up disabled; S ran the full 64-round loop. Both can be true:
legacy `segment_a` fails on pass 1 with 23 refusals and is *rescued* by rip-up
at round 25, while relaxed `segment_a` is never rescued. Under that reading,
V's "never on any layout" is an over-claim from one-pass data, and S's "the
negotiation that sufficed for legacy does not suffice for anything denser" is
the compatible statement.

**ANSWERED, 2026-08-15, after this document was first written.** The missing
experiment -- the rip-up loop on legacy `segment_a` anchors -- was run, and S's
reading is the correct one:

```
legacy anchors:  segment_a ROUTES through this router   (~110s)
relaxed anchors: segment_a FAILS
```

The harness is in the tree, so this is re-runnable rather than reported:
`planner::measure_whether_the_legacy_placement_routes_through_this_router`
(`#[ignore]`d, about two minutes). It discards the emitter's own routes and
re-lays every net with this router, so what it measures is this router on that
placement.

So **V's one-pass census was measuring something real and drawing a conclusion
its own method could not support.** Both placements are hard on pass one -- 23
refusals each, which is V's number and it reproduces -- but rip-up *rescues*
legacy and never rescues the relaxed layout. "Never on any layout" is false.

What that settles, and it is the question this section exists to answer: **the
router is not independently broken at this size, and the relaxation is not
innocent.** The relaxation halved the anchor box (`segment_a` 29,435 -> 8,099)
and the negotiation that sufficed at the old density does not suffice at the
new one. The fix is *shared*. §4 still asks for a negotiating router rather
than a looser placer -- that recommendation is unchanged -- but it now rests on
a measurement instead of on a preference, and anyone arguing for the placer
side has to beat 110 seconds of evidence rather than an assumption.

Still not measured: the same comparison on `seven_segment`, which the harness
omits deliberately because it would put it past ten minutes.

Note also that V's `segment_a` numbers are the case *against* the placement
being the whole story, and V's `seven_segment` numbers are the case *for* it
(55 refused relaxed vs 35 legacy). V's own two rows disagree.

**Disagreement 2 — does area help?**

Both say "not sufficient", and they measured it differently and got the same
shape of answer.

- S scaled every relaxed anchor uniformly 2x, shape held: plane 177×181 =
  32,037 — **larger than legacy's 23,220** — and it still fails, `gave up after
  64 rounds in 261s; deepest round laid 43/47`. Better than 36/47. Not enough.
- S separately raised `relax::project::reservation`, the halo whose own doc
  pre-registers this exact failure mode ("If placements come out routable but
  wasteful, or compact but unroutable, this is what was wrong"). At 2x:
  `segment_a` box 91×93 = 8,463, route ERR 76.9s; and `full_adder` **regressed
  from verify-Ok** to `signal-strength violation: net 'a' never delivers a
  non-zero signal to gate 'g8'`. At 4x: box 102×101 = 10,302, ERR 226.2s, and
  `full_adder` fails with a `torch-merge violation`. The knob barely moves area
  — 8,099 → 8,463 → 10,302 — because it adds only `scale × degree / 4` on a
  base of 3.0.
- V's comparison is 3x the area and twice the spacing at `segment_a` failing
  identically.

So: **area is not the lever**, and the halo knob is worse than not-a-lever
because it breaks circuits that work today. The reservation halo's own doc
comment should be updated to say this was tested and refuted.

**Disagreement 3 — is `keep_out` over-claiming?**

S: ruled out — "`keep_out` is the shipped conservative `dust_reach` rule with
no excess found". V: explicitly **NOT MEASURED** — read `dust_reach` and
traced one concrete case where the exclusion is physically right, but did not
measure a case where a refinement would change the answer, nor how many
refusals a refinement would remove. V also notes the asymmetry: `dust_reach`
gates the up-step on the neighbour being solid and the down-step on it not
being, while `keep_out` takes both arms unconditionally.

Treat this as **open**, weighted toward V: "no excess found" is a weaker
statement than it reads, and V removing `keep_out`'s vertical arm was the
*only* modification of any rule that found a path (15 cells) on the failures
it dissected.

---

## 4. Root cause, with confidence

**CONFIRMED (measured, by both investigations, from different angles).** The
router's refusal model and its outer loop, together, are the mechanism:

> A laid net sterilises a three-cell-wide corridor via `keep_out`; a blocked
> net cannot use a contested cell at any price, so it cannot signal which cell
> it needed; the only feedback is an indiscriminate bounding-box charge that
> never decays; and the loop has no convergence condition, so it oscillates
> for 64 rounds — 22 distinct nets taking turns on `segment_a`, four nets in
> one region on `seven_segment` — and reports whichever net failed last.

**CONFIRMED.** At the moment of failure the corridor is *genuinely* blocked:
no single rule relaxed opens `g32`'s pocket (S turned off owner,
floor-over-conductor, staircase clearance, self-obstruction and the congestion
price one at a time; the pocket stayed 256–259 cells every time). It is not
one cell short.

**CONFIRMED.** The blockage is the router's own doing. All 83 `segment_a`
branches route individually against primitives and pre-claims alone.

**INFERENCE, not measured.** That the relaxed placement's density is a
*contributing* cause. The supporting facts are real (legacy anchors route at
round 25, relaxed do not; nearest-neighbour median 7 vs 13) and the
counter-facts are equally real (2x uniform scaling still fails; V's one-pass
census is identical on both layouts). Nobody has measured **peak local
congestion** — only the average (union of isolated shortest paths × 3: 42% of
the plane relaxed vs 38% legacy). The hot-spot number is the one that would
decide between "fix the router" and "fix the placement", and it does not exist.

**INFERENCE.** That the legacy layout is routable *because* rows and columns
give parallel corridors while a relaxed blob gives routes at arbitrary angles.
Plausible, repeated by both investigations, and measured by neither.

---

## 5. Candidate fixes

Five, ordered by how much evidence stands behind them rather than by cost.

### 5.1 Negotiated congestion (PathFinder) — replace `route_every_net`

Foreign conductors and their `keep_out` halo become a **price** inside
`deterministic_astar` rather than a refusal inside `anchor_is_free_for`. Each
iteration rips up and reroutes every net against current occupancy; present
cost grows within an iteration, history cost accumulates across them; the loop
ends when no cell is contested. The rules that protect a net from *itself*
stay hard: gate bodies, floor-destroys-conductor, staircase clearance,
`self_obstructs`, `y ≥ PLANNER_Y`.

**Why:** it removes the exact measured mechanism. Under pricing the net with
an alternative moves and the net without one keeps the cell.

**TRIED — prototyped as a probe, not a router (V).** Present cost doubling per
iteration capped at 4096, history +1 per contested cell per iteration; kept
only the hard rules above. On the real anchors:

```
full_adder    : 38 -> 6 -> 0 contested            CONVERGED at iteration 2
segment_a     : 260 -> 66 -> 37 -> 21 -> 9 -> 6 -> 2 -> 0
                                                  CONVERGED at iteration 7, 1737 cells of wire
seven_segment : 724 -> 275 -> 148 -> ... -> 6 (iter 12), then oscillates 6..15 through iter 24
```

0 branches unreachable at every iteration of all three. `segment_a` converging
is the headline: that is what 64 rip-up rounds and 120 net orderings cannot
do. `seven_segment` stalling at 6–15 contested cells out of ~4,302 cells of
wire is **an untuned schedule, not evidence that `seven_segment` is
unroutable** — the schedule was never swept.

**Cost: largest of the options.** `anchor_is_free_for` (1611) splits into hard
rules plus a cost function; `deterministic_astar` (1329) takes the cost;
`route_every_net` (2361) and `Congestion` (1536–1571) are replaced;
`Reservation` gains a notion of temporary overuse, which today
`Reservation::insert`'s `or_insert` and `anchor_is_free_for` both forbid
outright.

**The probe is necessary evidence, not sufficient.** It deliberately dropped:
the strength budget (`realise_branch_from`, repeater placement, the `carries`
test), staircase clearance, the terminal guard cells, and the socket approach
pre-claim. Two of `seven_segment`'s 64 rounds today fail on the *strength
budget*, not on reachability ("the route to `g62.in[1]` decays to nothing
before it arrives"), so that constraint is live and the probe never faced it.
A converged corridor set proves the geometry admits disjoint corridors. It
does not prove the shipping router would accept them.

**Also at risk:** `PlanCandidate::live_reservation` (1161) and
`optimise`/`try_move` share the reservation semantics; `verify_candidate` and
the equivalence tests assume routes never share. Runtime is a real question —
25 `seven_segment` iterations took ~34s in the probe against ~21s for 64
rip-up rounds today, and the probe skips what a real router pays for.

### 5.2 Route each multi-sink net as a tree, not a star

Seed the A\* frontier with **every cell already laid for that net** at
`travelled = 0`, rather than only `source` (1357), so a later branch grows off
the trunk wherever it is nearest.

**Why:** today up to eight dust lines radiate from one cell, each sterilising
a three-wide band — precisely the structure that carves the plane into wedges
others cannot cross — and the net is all-or-nothing. Measured (S):
`segment_a`'s eight multi-sink nets (`g4` 8 sinks; `g3`/`g5`/`g6` 6; `g1`/
`g2`/`g9` 5; `g0` 3; the other 39 nets have 1 each; 83 branches total) are
**17% of the nets and took 69% of the round failures** — 177 of 256, with
`g2` 59, `g4` 53, `g0` 43, `g1` 22. A tree lays strictly less dust than a star
for the same sinks, so it frees corridors rather than asking others to move.

**NOT TRIED.** Only the cheap half was prototyped — reordering branches within
a net. Nearest-first: `segment_a` ERR 29.9s at `(77,1,105) → (103,1,100)`,
`seven_segment` ERR 16.8s. Farthest-first: `segment_a` ERR 58.8s at
`(84,1,110) → (75,1,103)`, `seven_segment` ERR 23.0s. `and4` and `full_adder`
route under both. Ordering is not it; the tree itself is the proposal.

**Cost: medium, confined to `route_in_order`.** The multi-source seed is easy;
the **strength bookkeeping is the real work**. `realise_branch_from` (991) is
called with the strength carried to the branch point, recomputed today by
walking the shared prefix from full strength at the source. A branch starting
mid-tree needs a per-cell carried-strength map maintained as the tree is laid.
**This risks `verify_candidate`**: the 2x-scale and 4x-halo prototypes both
turned route failures into signal-strength and torch-merge violations, so any
change that lengthens a branch's electrical path from its refresh point can
break circuits that route today. `and4` and `full_adder` must stay green.

### 5.3 A real second layer — let a route cross over another

**Why:** `anchor_is_free_for` refuses any cell whose `y-1` conducts (1638),
correctly, because realisation writes a stone floor there. The consequence is
that the gate plane is effectively single-layer and **every laid net is a
wall**. `g32`'s pocket is bounded by five other nets' dust (`g4` 280 refusals,
`g1` 265, `g2` 191, `g23` 184, `g0` 105) with no way over any of them. That is
why area alone does not rescue it: more plane means more room to lay walls in,
not more ways through them. V's number for the same mechanism: `floor below is
a conductor` was the second-largest non-bounds refusal class in every failed
search (1,011 of 6,583 for `g16`; 2,058 for `g5`), and **14 of
`seven_segment`'s 55 one-pass refusals die within 2–5 cells of the goal**,
unable to land because near a sink `y = 1` is full.

**NOT TRIED.** The adjacent measurement is that height alone does nothing:
`CLIMB 3→9` reports FOUND and its path has 9 faults against the real rules.
Height is only useful if a route may run *over* another, and today it may not.

**Cost: large, and physical rather than algorithmic.** It needs a block-level
crossing convention the four physical invariants and the simulator both
accept, and the strength budget has to pay for it — a staircase cell can hold
no repeater, which is why `CLIMB` is 3 and why `realise_branch_from` forces a
refresh before every climb. Touches `emit_routes`, `realise_branch_from`,
`staircase_clearance` (1590) and the invariants together. Would also unblock
`crowding_produces_height`, whose blocker is one full_adder route,
`(38, 1, 124)` → `(40, 3, 124)`.

### 5.4 Report the failure that recurs, not the last round's

`route_every_net` (2402) returns `last`. Keep a per-`(net, corridor)` tally
across rounds and report the most frequent.

**Why:** not a routing fix — a diagnosis fix, and it is why this branch's
final brief pointed at the wrong net. See §1.1. ~15 lines, plus whatever
asserts on the message text. **Do this first**; it is cheap and every
subsequent measurement is easier to read.

**NOT TRIED as a code change.** The claim itself is measured: the 64-round log
and the 256-round tally are in §1.1.

### 5.5 Pre-claim each source pin's exit cells — **REFUTED, do not retry**

Symmetric to the approach-cell pre-claim `route_in_order` already makes for
every sink (2455 ff.). It was the obvious reading of the largest measured
failure class: **41 of `seven_segment`'s 55 one-pass refusals are a source pin
sealed by another net's dust laid one cell away**, and every pin has 6 legal
first steps before any route is laid (histogram `{6: 80, 4: 1}`, zero sealed).
A final-round example: `g0` at `(83,1,106)` settles **1 cell** — its own gate
body takes one exit, and the other three are refused by `keep_out` seeing
`g2`'s dust at `(84,1,105)` and `d1`'s at `(83,1,108)`.

**PROTOTYPED AND REFUTED (V).** Claiming all four same-level neighbours of
every source pin as the net's own conductor: `full_adder` 4 → 4 refused;
`segment_a` 23 → 22; `seven_segment` 55 → 53. Across 120 random orders it was
neutral-to-worse (`segment_a` median 20 → 20, min 14 → 13; `full_adder` min
0 → 2, and it destroyed the one clean order out of 120). **The claim costs as
much room as it protects**: pushing every other net two cells away from every
pin re-seals elsewhere what it unseals here.

Recorded because it is the intuitive fix and the measurement should stop the
next person.

### 5.6 Things measured and refuted, listed so nobody spends a day on them

| idea | measurement |
|---|---|
| Raise `RIP_UP_ROUNDS` from 64 | 256 rounds, 498s, `stopped_early = false`, deepest round 36/47. Not the budget. |
| Raise `CLIMB` above 3 | `climb = 8`: 4,873 → 12,828 settled, no path, closest still 2. The found paths at `CLIMB 9` are illegal (9 faults). |
| Widen the corridor `margin` | `margin + 40`: 52,189 settled, no path, closest still 2. |
| Better net order | 120 shuffles: `segment_a` **0 of 120 clean**. No ordering rule of any kind fixes it. Keep it as a cheap first move *inside* a negotiated router; it cannot be the router. |
| Raise `relax::project::reservation` | 2x and 4x both still fail *and* regress `full_adder` from verify-Ok to a signal-strength then a torch-merge violation. |
| Scale the whole placement 2x | Plane 32,037 — bigger than legacy — deepest round 43/47. Helps, insufficient. |
| Lift the `y: start.y.min(goal.y)` floor | Correct as written. Every cell stands on a floor one below it and the gate plane is on the lowest floor there is; the variant that ignores it routes through `y = -1`. |

---

### 5.7 Congestion-driven placement — feed measured congestion back into the springs

**Untried, and the reason it is untried is a flaw in how this document was
assembled rather than a judgement about the idea.**

Every fix above is on the router side. The placement side was tried exactly
once, as **uniform 2x scaling** (§3.2, Disagreement 2), which is the crudest
possible version of it and does not test the idea at all. So §6's preference
was formed by comparing a good router experiment against a bad placer
experiment. That is not a fair comparison and the recommendation should not be
read as if it were.

The sharper framing, which nobody wrote down while the plan was running: **the
relaxation already *is* a router.** `pulls` are the nets, and minimising spring
energy is the continuous relaxation of "make the wires short". It even models
routing *space*: `required_separations` charges each body
`CONDUCTOR_CLEARANCE + reservation(d) + SNAP_MARGIN`, and `reservation(d) =
d/4` is a per-body area allowance proportional to routed degree.

What that model gets wrong is the *shape* of the allowance, not its absence:

- It is an **isotropic ring** around each body -- "this body has `d` wires, so
  give it `d/4` in every direction". A real wire is directional and needs room
  along the source-to-sink corridor, not in a ring.
- It is **per-body**, so it cannot see that two nets' corridors *cross*. Ring
  demand is a density estimate; the failures are congestion, which is where
  density estimates are weakest.

That also explains the measurement that otherwise looks strange -- uniform
scaling helps only marginally (36/47 to 43/47) even at an area *larger* than
legacy's. Scaling widens the channels but lengthens every route by the same
factor, so cells-demanded rises with area and the density ratio is roughly
scale-invariant. **(INFERENCE, not measured: nobody has instrumented
demand-versus-capacity across a scale sweep.)**

**The experiment.** This is standard congestion-driven analytical placement --
trial-route, build a congestion map, inflate the cells in hot regions, re-place
-- and the plumbing for the inflation step already exists:
`required_separations` returns a **per-body `Vec<f64>`** and `project` takes it
as a parameter. So a prototype needs no new mechanism in the projection at all:

1. Relax, snap, attempt `route_every_net`.
2. On failure, collect per-region contention from the router (which cells were
   refused, for whom, and where the frontier died).
3. Inflate `required[body]` for bodies in or adjacent to hot regions.
4. Re-relax with the inflated vector and repeat, a handful of iterations.

**What would settle it:** does `segment_a` route after N iterations, and at
what area? If it does, the fix is shared in a much more concrete sense than §6
currently claims, and the two sides can be traded off. If it does not, §6's
preference is right and it will then be right *for a measured reason*.

## 6. Recommendation

**Do 5.4 first** (cheap, makes everything after it legible), **then 5.1**, with
5.2 as the fallback if 5.1's strength-budget interaction proves worse than its
convergence is good.

**Caveat added after this section was written: run 5.7 before treating this
ordering as settled.** 5.1 is preferred over the placement side on evidence
that does not support the comparison -- see 5.7.

5.1 is the only candidate with a measurement showing `segment_a` reaching zero
contested cells. That measurement is a probe with four constraints dropped, so
the first milestone of the work is **not** "ship a negotiated router" — it is
"re-run the probe with the strength budget, staircase clearance, the terminal
guard cells and the socket pre-claim restored, and see whether `segment_a`
still converges". If it does not, this document's recommendation is wrong and
the answer is 5.3, which is larger and touches physics.

---

## 7. A blocker that is not routing, and will still be there afterwards

**`verilog:seven_segment` does not place.** `projection deadlocked: bodies 2
and 3 cannot be 1.250 further apart and stay welded`, out of `relax::project`,
in 0.9s. Fixing routing will not touch this, and the Task 13 condition names
all six circuits, so it must be scoped alongside.

**ROOT CAUSE (measured, with a five-gate reduction and a four-gate control in
the tree).** A wire merge whose branch needs isolating gets a repeater welded
into the junction's socket (`relax::Weld::AtSocket`). `project::exempt` exempts
a **welded pair**; two repeaters welded into the *same* junction's two sockets
are each welded to the junction and to nothing else, so they are not exempt
from each other, while `satisfy` pins them one cell either side of it.
Measured on the minimal netlist: junction at `[34,1,5]`, sockets at `[33,1,5]`
and `[35,1,5]`, required separation 3.250 each, `worst_violation`
`{left: 3, right: 4, shortfall: 1.25}` — 2.0 apart where 3.25 is required, and
neither can move without breaking its own weld. `project` is right to report a
contradiction rather than spin on it.

The correlation across the tree, which is what names the trigger:

| netlist | gates | merges | welds | junctions with **both** sockets welded | relax |
|---|---|---|---|---|---|
| and4 | 7 | 0 | 0 | 0 | Ok, 8 steps |
| full_adder | 22 | 0 | 0 | 0 | Ok, 9 steps |
| segment_a | 46 | 0 | 0 | 0 | Ok, 11 steps |
| seven_segment | 84 | 0 | 0 | 0 | Ok, 11 steps |
| verilog:and4 (`lower`) | 9 | 0 | 0 | 0 | Ok, 8 steps |
| verilog:and4 (`lower_optimised`) | 7 | **2** | 0 | 0 | Ok, 8 steps |
| verilog:seven_segment (`lower`) | 56 | 17 | 20 | **7** | **deadlocked at 14/15** |
| verilog:seven_segment (`lower_optimised`) | 47 | 17 | 23 | **9** | **deadlocked at 2/3** |
| minimal (5 gates) | 5 | 1 | 2 | **1** | **deadlocked at 3/4** |
| control (4 gates) | 4 | 1 | 1 | 0 | Ok, 8 steps |

`verilog:and4` under `lower_optimised` is the row that makes the column the
right one: two merges, zero welds, places fine. **Merges are not the trigger;
two isolating repeaters on one junction is.** Each deadlocking lowering names
the first double-socket junction in its own list.

Minimal reproduction, five gates:
`na = NOR(a)`, `nb = NOR(b)`, `m = OR(na, nb)`, `ka = NOR(na)`, `kb = NOR(nb)`
— both merge branches fan out, so both need isolating: 9 bodies, 2 welds on
one junction, deadlocks at bodies 3/4. Control: drop `kb`, so one branch is
isolated — 7 bodies, 1 weld, `relax` Ok, `compile_planned` Ok.

**Why no test caught it:** every hand-written circuit in this tree lowers to
pure NOR — not one `merge2` among them — and the only Verilog circuit whose
lowering produces a merge that needs *isolating* is the decoder.
`project`'s own `constraints_that_contradict_are_reported_rather_than_spun_on`
builds this exact two-weld shape by hand and forces it with a separation of
9.0, so the *mechanism* was modelled from the start; what had never been
measured is that a circuit this project ships reaches it at the real
separation.

**Regression risk, and it is the reason this cannot be deferred silently:** the
minimal netlist compiles through today's legacy `compile()` (measured: `legacy
compile Ok`), and `tests/verilog_frontend.rs`'s
`optimised_lowering_preserves_every_verilog_decoder_vector` passes at this
HEAD (47 gates, 10,088 blocks, 80 game ticks, every vector). Pointing
`compile()` at the planner today would turn both from **compiling** into **not
placing**.

**No fix is specified here.** The shape of one is visible — `exempt` could
relate two bodies welded to a common third, or `satisfy` could place both
sockets' repeaters where the junction's own geometry permits — but neither has
been designed or measured, and guessing in a spec is how the four wrong
explanations this branch caught got written. This needs its own brief.

---

## 8. What we do not know

The habit of writing this section down is what caught four wrong explanations
in twelve tasks. In rough order of how much the next task depends on the
answer.

1. **Whether a legal simultaneous routing of the relaxed placement exists at
   all.** Every branch routes alone (83/83) and the router never lays more than
   36 of 47. Those two facts leave the question open. V's probe converging
   `segment_a` to zero contested cells is the strongest evidence that one
   exists, and it dropped four constraints.
2. **Whether the placement is implicated.** §3.2, disagreement 1. Neither
   investigation ran the other's experiment, and both harnesses are gone.
3. **Peak local congestion.** Only the average was measured (42% relaxed vs
   38% legacy). The hot-spot number is what would decide between fixing the
   router and fixing the placement, and nobody has it.
4. **Whether a negotiated router's converged corridors survive the rules the
   probe dropped** — the strength budget above all, which two of
   `seven_segment`'s 64 rounds fail on today for reachability-independent
   reasons.
5. **Whether `seven_segment` converges under a tuned negotiation schedule.**
   It stalled at 6–15 contested cells with one untuned schedule. The schedule
   was not swept, so "`seven_segment` needs more than negotiation" is **not**
   established.
6. **Whether `keep_out`'s two vertical arms over-claim** against
   `connectivity::dust_reach`, which gates the up-step on the neighbour being
   solid and the down-step on it not being, while `keep_out` takes both
   unconditionally. S found no excess; V says not measured. Removing that arm
   was the only rule change that found a path on the failures V dissected.

   **ANSWERED 2026-08-16, by measurement, and the answer is "no excess to
   read".** `docs/derived/dust-join-relation.md` is a table generated by
   running the `Simulator` — two dust cells at every offset in
   `|dx|<=2, |dy|<=1, |dz|<=2`, one driven through a redstone-block feed, each
   reading differenced against a control with the driven cell deleted.
   Generator and assertions in `tests/dust_join_relation.rs`.

   - **All twelve of `keep_out`'s cells really join**, both directions, in the
     shape a compiled world presents (every dust cell on a stone floor,
     nothing laid above it). In the world the router builds, `keep_out` is
     exact, not conservative.
   - The eight vertical cells are nevertheless **conditional**, and on exactly
     one cell: the one **directly above the lower conductor**. Two independent
     properties of it are read with opposite polarity — the higher cell
     descends when that cell does *not* `supports_dust_step`, the lower cell
     climbs when it does *not* `is_conductive`. The four same-layer cells are
     unconditional; no content of any cell makes them stop joining.
   - The question's own summary of `dust_reach` was half right and is corrected
     here: the up-step gates on the neighbour supporting a **dust step** (not on
     being *solid* — glass qualifies and does not conduct) **and** on the cell
     above the source not conducting, which is a second cell the question does
     not mention.
   - Because the descend and climb arms read the same cell through two
     different predicates, the relation is genuinely **one-way** for any block
     that is a full cube without conducting. Nothing this compiler writes is
     such a block, so no compiled circuit can have hit it.

   **The exactness IS now reachable from the reservation (§8.16, built
   2026-08-16) and the conservatism is load-bearing anyway (§8.17): the join
   relation is not the only rule those twelve cells enforce, and the one that
   refuted it — a repeater powering the other cell's floor block — has no lid
   in it at all.**
7. **Whether `deterministic_astar` can miss a legal path.** `self_obstructs`
   (1488) consults `previous` (1363), a single global parent map rather than
   the path actually taken to a cell, so feasibility is path-dependent and the
   textbook A\* completeness argument does not apply as written. Measured: the
   frontier empties, so the search is exhaustive over what it *reaches*. Not
   measured: whether a legal path can exist outside that reachable set.
8. **Why the legacy layout is routable in structural terms.** Measured that it
   is (round 25) and that it is 2.9x larger with uniform spacing (min 13,
   median 13, max 14, against relaxed's 5/7/31). The rows-give-parallel-
   corridors explanation is a hypothesis, not a measurement.
9. **`seven_segment`'s failure was never dissected the way `segment_a`'s was.**
   V has its per-branch census, refusal histograms and two fully-opened
   failures; nobody has its pocket-and-wall analysis. All of §4's pocket
   mechanism is `segment_a`'s.
10. **Whether the two fully-dissected failures are representative.** V opened
    the twelve-steps-out and `keep_out` detail for `g16` round 0 and `g0`
    round 63 only, out of 55.
11. **Whether `cells_within` (1847) ignoring the `lo.y`/`hi.y` it computes
    matters.** Dead computation at minimum; nobody measured whether restricting
    the charge to the corridor's y range changes any round.
12. **Whether `live_reservation` (1161) and `optimise`/`try_move` need the same
    change.** They share the reservation model, are off the shipping path, and
    were not run.
13. **Runtime of a negotiated router on the shipping path.** ~34s for 25
    `seven_segment` probe iterations vs ~21s for 64 rip-up rounds today, and
    the probe skips the strength budget and terminal machinery.
14. **Both Verilog circuits' routing.** Neither was put through any of §3–§5.
    `verilog:seven_segment` still does not place (§7), so its routing is
    unknown either way.
15. **One unreproduced anomaly.** Two `cargo test` runs of the >64-round case
    died at round ~174 with exit `0xffffffff` and no panic; the identical
    binary copied out and run standalone completed all 256 rounds. Not
    reproduced in isolation, so it is recorded rather than reported as a router
    defect.
16. **Whether an exact `keep_out` is even expressible at plan time.**
    **ANSWERED 2026-08-16 by building it: yes for the reading, and the answer
    does not matter, because the join relation is not the only rule those
    twelve cells enforce.** See §8.17 for what that costs.

    The exact rule needs to classify one cell — the one above the lower
    conductor — into three states: *conductor*, *opaque and step-supporting*
    (a stone floor), and *neither* (air, and every component that is not a
    full cube). `Occupancy` had **two** values, and `Solid` was the catch-all
    for "occupied, not a conductor". That is now a solved problem, and both
    tests that recorded it unsolved were rewritten to record the opposite:

    - `the_reservation_tells_a_stone_riser_from_a_mandatory_air_cell` (was
      `..._identically`) — `reserve_path` used to write *both* cells of a
      climb's `staircase_clearance` as `Occupancy::Solid` under the same
      `stair:` owner, and one of them has to become stone while the other has
      to stay air. It now writes the riser `Occupancy::Stone` and the
      headroom `Occupancy::Solid`. The obstacle was the type, not the world.
    - `the_lid_cell_can_be_open_when_asked_and_stone_one_net_later` (was
      `..._solid_one_net_later`) — the lid cell is still frequently **not in
      the reservation at all** when `anchor_is_free_for` is asked, and
      whether it ends up stone still depends on a net that has not been
      routed yet. **This half survives, and it is survivable**: unclaimed
      reads as "not sealed", which *refuses*, so a lid that becomes stone
      later costs a route and cannot cause a short.

    The commitment the third state implies is held rather than assumed:
    `anchor_is_free_for` now refuses a cell reserved `Occupancy::Stone` to
    **every** owner, this net's own included, so nothing routed later can turn
    a lid back into air. `wire_may_not_be_laid_where_the_plan_committed_stone`
    asserts it; deleting the guard turns it red. That is also a defect fix in
    its own right — `emit_routes` writes floor-then-block per anchor in route
    order, so an anchor landing on an earlier anchor's floor silently
    overwrote it and left the cell above standing on dust, and
    `Reservation::insert` being `or_insert` meant the reservation went on
    reporting a floor.

    The rule itself is `keep_out_against`, and it is checked against the
    simulator rather than against another plan-time rule:
    `the_exact_rule_matches_the_simulator_on_every_keep_out_offset` builds the
    world each reservation implies and compares the rule's verdict against
    `dust_connections` — the walk `verify_connectivity` itself makes — over 28
    rows: four same-layer offsets, and eight vertical ones in each of three
    lid states (unclaimed, claimed-but-not-stone, stone). Refused must mean
    joined and admitted must mean apart. It fails in **both** directions on
    injection: reverting the rule to all twelve prints `offset (-1, 1, 0) with
    the lid stone: the rule says refuse, the simulator says apart`; letting the
    lid test accept `Solid` as well as `Stone` prints `offset (-1, 1, 0) with
    the lid claimed, not stone: the rule says admit, the simulator says
    joined`.

17. **What the twelve cells enforce that is not the join relation.**
    **NEW 2026-08-16, and it is why §8.16's answer did not ship.**

    `keep_out_against` is exact about `dust_connections`, and
    `dust_connections` is dust against dust. The vertical cells carry at least
    one other hazard, and the lid is irrelevant to it. Measured, electrically,
    against controls, in
    `a_stone_lid_seals_a_dust_pair_and_does_not_seal_a_repeater`:

    | lower cell | lid | upper dust reads |
    |---|---|---|
    | dust | air | 11 |
    | dust | **stone** | **0** — the derivation is right |
    | repeater aimed at the upper cell's floor | air | 15 |
    | repeater aimed at the upper cell's floor | **stone** | **15** — the lid does nothing |

    A repeater strongly powers the block in front of it; that block is the
    floor the other cell stands on; a powered floor drives the dust standing on
    it. No lid appears anywhere in that path. And `Occupancy::Wire` covers both
    materials, because `realise_branch_from` decides which cells of a laid path
    become repeaters from a strength budget computed **after** `reserve_path`
    has written the reservation the rule reads. So the query is asked before
    the answer exists — decidable in principle, undecided in fact, and in the
    unsafe direction.

    Wiring it in anyway was measured end to end and refused by the invariants.
    On `plan_from_netlist`'s full_adder the rule permitted exactly two vertical
    pairs — `(37,2,124)`/`(37,1,125)` and `(43,2,118)`/`(42,1,118)` — and both
    were then confirmed **electrically clean in isolation**, so the two
    adjacencies themselves were not the defect. The circuit still came out with
    **2 of 8 truth-table rows wrong** (`011`, `101`) and
    `realise_and_verify` refused it:

    ```
    TorchMergeViolation { gate: "g2", reason: ForeignNetReachesSupport {
        torch: (40, 1, 131), support: (40, 1, 132), net: "g3" } }
    ```

    Traced through `net_reach`'s own walk: g3's terminal repeater at
    `(40,1,124)` strongly powers the block in front of it, and g0's dust at
    `(40,1,126)` sits on the far side of that block, so g3 drives g0's whole
    wire and g0 legitimately reaches g2's support. That is a hazard of the
    **terminal** model, exposed by the reroute rather than caused by it.
    **NOT MEASURED: whether any other perturbation of this router would also
    expose it** — no control reroute was constructed.

    Two things follow, and they point in opposite directions:

    - The function is kept, tested and `#[cfg(test)]`, not deleted. The
      derivation is right; it is the *premise set* that is short. The same
      function becomes shippable the moment the reservation also records which
      routed cells realise as repeaters — which `Growth::branch` and
      `route_in_order` both have in hand one line after they call
      `reserve_path`. **NOT BUILT, NOT MEASURED.**
    - It would still buy nothing today. With the rule active, growth's outcome
      is **unchanged on all four hand-written circuits** — and4 7/7 verifying
      at 236 blocks and 18 ticks, full_adder wedged at `g9` after 7/22,
      `segment_a` and `seven_segment` at `g8` after 18 — and the windowed
      solver still returns **UNSAT at every margin, cap-independent, with the
      same two-group core** on both wedges. The wedge does not depend on the
      vertical arms in the direction the loosening moves them.

---

## 9. What is in the tree, and what is not

**In the tree** (uncommitted at `0ac2688`, `src/compile/planner.rs`'s test
module — commit them or they are lost):

- `the_six_condition_circuits_stage_by_stage` — §1's table, all six circuits,
  never stops at the first failure. ~55s.
- `the_smallest_netlist_that_deadlocks_the_projection` — §7's two tables, the
  five-gate reduction and the four-gate control.

**Not in the tree.** Every number in §3, §4 and §5 came from scratch
instrumentation appended to `src/compile/planner.rs`'s test module (private
items are only nameable there) plus temporary files in `tests/`, all of it
reverted. Scratch sources for V's half survive outside the repo at
`…/scratchpad/diag{2,3,4,5,6,7}.rs` and `…/scratchpad/census.txt`, which is a
Windows temp directory and not a durable location.

**Whoever picks this up should re-add the harness rather than trust these
numbers.** The production behaviour they measured was stable across runs —
`segment_a` failed at `(99, 1, 97) → (122, 1, 89)` on every baseline run
including mine today — but by this branch's own rule 4, a cited number needs a
reproducible method in the tree, and these do not have one.

One process hazard worth recording: another process was editing this same
worktree concurrently during S's session and reverted `src/compile/planner.rs`
under it mid-session. Take a copy before instrumenting.

---

## 10. How we will know it worked

The acceptance condition is unchanged and is **not** softened by this
document. From `2026-08-13-spring-placement.md`:

> All four hand-written circuits and both Verilog circuits must place, route,
> verify, and match their truth tables. Not one fewer.

Concretely, `the_six_condition_circuits_stage_by_stage` prints six rows of
`Ok`, and then Task 13 of the spring-placement plan can be attempted. Both
walls have to fall: §5's routing work **and** §7's projection deadlock. Either
one alone leaves the condition unmet.

Non-negotiable regressions to watch, each already observed once while probing:

- `and4` and `full_adder` must keep routing **and verifying**. Two separate
  prototypes turned route failures into `signal-strength violation` and
  `torch-merge violation` on `full_adder`.
- `tests/verilog_frontend.rs`'s
  `optimised_lowering_preserves_every_verilog_decoder_vector` must keep
  passing — 47 gates, 10,088 blocks, 80 game ticks.
- The four pinned block counts (472 / 1,784 / 6,416 / 16,244) do not move
  until the switchover, because `compile()` does not move until then.

---

## 11. Out of scope

- **Changing placement.** The relaxation stays as Tasks 5–12 left it. Changing
  routing and placement together would leave nothing to attribute a regression
  to — which is the same reason the spring-placement spec put routing out of
  *its* scope. §7 is the one exception and it is a defect fix, not a redesign.
- **Timing-driven routing.** Deferred until a measurement says plain
  wirelength misses the 15-cell constraint often enough to matter.
- **`optimise` and `try_move`.** Off the shipping path. §8 item 12 records that
  they share the reservation model and may need the same change; establishing
  that is part of the routing work, changing them is not.
- **Design H / the DFF.** `Weld::BesideAt` and cell cohesion still have no
  caller.
- **Teaching the simulator a lever's `face`.** Recorded in the ledger as its
  own task; it changes the oracle every circuit here is verified against.
