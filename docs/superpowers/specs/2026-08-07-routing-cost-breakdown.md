# Routing-cost breakdown

Status: measurement only. No placement or routing behaviour changed; no
follow-up fix chosen. See "Conclusion" for what this points at, but that
choice is deliberately left to a separate task.

## The question

`src/timing/mod.rs` measures ~90% of settle time as routing rather than
logic (7.2x-9.2x the logic-depth bound across the four reference circuits).
`src/compile/mod.rs` routes every netlist edge through a column (north-south,
`GATE_Y`), a pair of ramps (`GATE_Y` <-> `TRACK_Y`), a track (east-west,
`TRACK_Y`), and a gate-entry approach (into the consuming socket). This asks:
of the ~8 repeaters per hop that ratio implies, which of those four parts
actually forces them?

A hypothesis was floated to test, not assume: left-edge track assignment
minimises track *count*, not track *distance* from the nets it serves, so a
net could end up on a track far from both endpoints, making the column run
dominate. The measurement below does not support that. What dominates is
something the hypothesis did not name: a routing tax that is structurally
fixed per channel crossing, independent of circuit size or track assignment.

## Method

### What "edge" means here

One real wire in the netlist: a specific driving signal (a primary input or
a gate's output) feeding one specific consuming gate's input. This is the
same unit `timing::critical_path` walks. A net that fans out to several gates
is several edges, one per consumer, each with its own physical route --
sharing whatever trunk wiring is common to them, the same way a tree's leaves
share its trunk.

Where a net's driver and consumer are more than one row apart, its route
threads through the intervening rows via a *feed-through*: one long column at
`GATE_Y` between one ramp-down and one ramp-up, passing underneath any tracks
it does not stop at (`compile::mod`'s "Placement and routing" section
documents this). An edge's `hops` count is how many channels it crosses this
way; `hops == 1` is the common case (a next-row consumer, no feed-through).

### Instrumentation

`src/compile/routing_stats.rs` is new. It adds no `&mut World` anywhere and
changes nothing about what `compile` places -- it recomputes the same
floorplan/net/track/Z geometry by calling `build_floorplan`, `build_nets`,
`reserve_columns`, `assign_tracks` and `layout_z` verbatim (the last three
newly extracted from `compile`'s body into named functions, pure refactor,
no logic changed), then reads the *already compiled* `World` along the
coordinates that geometry implies, classifying each cell as dust or repeater.

Two consequences:

- A bug in this module cannot change `compile`'s output, because it never
  writes to a `World`.
- Repeater counts are read from what is really in the world, not
  re-derived from `lay_dust_run`/`lay_track`'s placement rules in a way that
  could silently drift from them.

Two independent checks back this up:

1. **Whole-world match.** `distinct_totals` sums every physical segment
   exactly once (not once per edge that depends on it) and its repeater
   count is asserted equal to a straight scan of `compiled.world` for
   `BlockKind::Repeater`, for all four reference circuits
   (`compile::routing_stats::tests::distinct_totals_matches_the_world_for_every_reference_circuit`).
   It matches exactly.
2. **Settle-tick reconstruction.** Every repeater costs exactly 2 game
   ticks (`delay = 1` redstone tick, set uniformly in `compile::repeater`);
   redstone dust costs none (only torches, repeaters, and lamps are ever
   scheduled -- `redstone::simulator::schedule`). So:

   ```
   worst_settle_game_ticks =? logic_depth_bound_game_ticks
                              + 2 * (sum of critical-path edges' repeaters)
                              + 2   (the output lamp's own display delay)
   ```

   | circuit | bound | critical repeaters | reconstructed | measured |
   |---|---|---|---|---|
   | and4 | 8 | 24 | 8 + 48 + 2 = 58 | 58 |
   | full_adder | 20 | 61 | 20 + 122 + 2 = 144 | 144 |
   | segment_a | 18 | 64 | 18 + 128 + 2 = 148 | 148 |
   | seven_segment | 18 | 73 | 18 + 146 + 2 = 166 | 166 |

   Exact, for all four circuits. If the per-edge repeater attribution below
   were wrong by even one repeater anywhere on a critical path, this would
   not reconstruct exactly.

Given both checks land exactly, confidence in the numbers below is high.

`src/bin/routing_cost_report.rs` runs the analysis, plus the same
truth-table sweep `tests/reference_circuits.rs` already runs, and prints
everything that follows. Reproduce with
`cargo run --release --bin routing_cost_report`.

## Numbers

### Whole-circuit repeater count by part (each physical segment counted once)

| circuit | column | ramp | track | gate-entry | total |
|---|---|---|---|---|---|
| and4 | 2 (3.0%) | 44 (65.7%) | 2 (3.0%) | 19 (28.4%) | 67 |
| full_adder | 13 (6.3%) | 126 (60.9%) | 5 (2.4%) | 63 (30.4%) | 207 |
| segment_a | 12 (2.3%) | 280 (52.7%) | 67 (12.6%) | 172 (32.4%) | 531 |
| seven_segment | 37 (3.3%) | 502 (44.8%) | 188 (16.8%) | 393 (35.1%) | 1120 |

Ramp and gate-entry together are 75-94% of every repeater the router ever
places, in every circuit measured. Track's share grows with circuit width
(3.0% -> 16.8%). Column never rises above 6.3%.

### All edges: length / repeater distribution per part

`min / median / mean / max`, length in blocks, repeaters as a count.

**and4** (n=10 edges):

| part | length | repeaters |
|---|---|---|
| column | 2 / 3.0 / 6.30 / 36 | 0 / 0.0 / 0.20 / 2 |
| ramp | 8 / 8.0 / 8.80 / 16 | 4 / 4.0 / 4.40 / 8 |
| track | 2 / 2.0 / 7.20 / 30 | 0 / 0.0 / 0.20 / 1 |
| gate-entry | 6 / 6.0 / 7.10 / 11 | 1 / 2.0 / 1.90 / 2 |
| **total** | 18 / 21.0 / 29.40 / 88 | 6 / 6.0 / 6.70 / 13 |

**full_adder** (n=32 edges):

| part | length | repeaters |
|---|---|---|
| column | 2 / 3.0 / 11.09 / 110 | 0 / 0.0 / 0.41 / 7 |
| ramp | 8 / 8.0 / 9.00 / 16 | 4 / 4.0 / 4.50 / 8 |
| track | 2 / 6.0 / 9.44 / 32 | 0 / 0.0 / 0.16 / 1 |
| gate-entry | 2 / 11.0 / 9.62 / 16 | 1 / 2.0 / 1.97 / 2 |
| **total** | 18 / 29.0 / 39.16 / 163 | 5 / 6.0 / 7.03 / 17 |

**segment_a** (n=83 edges):

| part | length | repeaters |
|---|---|---|
| column | 2 / 12.0 / 18.80 / 87 | 0 / 0.0 / 0.70 / 5 |
| ramp | 8 / 8.0 / 9.93 / 16 | 4 / 4.0 / 4.96 / 8 |
| track | 2 / 16.0 / 24.43 / 102 | 0 / 1.0 / 1.22 / 6 |
| gate-entry | 2 / 11.0 / 13.14 / 31 | 1 / 2.0 / 2.07 / 3 |
| **total** | 18 / 53.0 / 66.30 / 185 | 5 / 7.0 / 8.95 / 18 |

**seven_segment** (n=156 edges; left-edge assignment: 38 tracks total, 4/4/7/1/3/10/1/7/1 per channel):

| part | length | repeaters |
|---|---|---|
| column | 2 / 12.0 / 19.79 / 91 | 0 / 0.0 / 0.85 / 5 |
| ramp | 8 / 8.0 / 9.18 / 16 | 4 / 4.0 / 4.59 / 8 |
| track | 2 / 30.0 / 32.67 / 128 | 0 / 2.0 / 1.76 / 8 |
| gate-entry | 2 / 16.0 / 19.49 / 51 | 1 / 2.0 / 2.52 / 5 |
| **total** | 18 / 80.5 / 81.13 / 204 | 6 / 9.0 / 9.72 / 20 |

Hop-count histogram across all edges (1 = next-row consumer, 2 = one
feed-through): and4 `{1: 9, 2: 1}`, full_adder `{1: 28, 2: 4}`, segment_a
`{1: 63, 2: 20}`, seven_segment `{1: 133, 2: 23}`. 13-15% of edges feed
through one row; none in any reference circuit feed through more than one.

### Critical-path edges only

The critical path is the specific chain `timing::critical_path` walks back
from the slowest-arriving output on the worst-case transition. Every edge on
every critical path measured has `hops == 1` -- feed-throughs exist (the
histogram above), but never landed on the critical path of any of these four
circuits.

| circuit | n | total repeaters (min/median/mean/max) | ramp share | gate-entry share | track share | column share |
|---|---|---|---|---|---|---|
| and4 | 4 | 6 / 6.0 / 6.00 / 6 | 16/24 (67%) | 7/24 (29%) | 1/24 (4%) | 0/24 (0%) |
| full_adder | 10 | 6 / 6.0 / 6.10 / 7 | 40/61 (66%) | 20/61 (33%) | 1/61 (2%) | 0/61 (0%) |
| segment_a | 9 | 6 / 6.0 / 7.11 / 10 | 36/64 (56%) | 19/64 (30%) | 9/64 (14%) | 0/64 (0%) |
| seven_segment | 9 | 6 / 8.0 / 8.11 / 15 | 36/73 (49%) | 20/73 (27%) | 14/73 (19%) | 3/73 (4%) |

Ramp is **exactly** 4 repeaters on every single critical-path edge in every
circuit (it is architecturally fixed: `RAMP_LENGTH / 2` per ramp, two ramps
per hop, unconditionally). Gate-entry is 1-3 repeaters per edge, close to
fixed. Together these two floor every hop at 5-6 repeaters before a single
block of track or column is walked. Track only becomes a meaningful
contributor as the circuit gets wider (2 of these circuits show it at
14-19% of critical repeaters; the other two show it near zero). Column is
zero on the critical path of three of the four circuits, and even in
seven_segment -- where one edge (`g25 -> g40.in[0]`, length 165, 15
repeaters: column 3, ramp 4, track 6, gate-entry 2) has a real column
contribution -- ramp and track individually still outweigh it on that same
edge.

seven_segment's critical path, edge by edge (repeaters; `-` = 0):

| edge | column | ramp | track | gate-entry | total |
|---|---|---|---|---|---|
| d1 -> g2.in[0] | - | 4 | 2 | 3 | 9 |
| g2 -> g6.in[0] | - | 4 | 2 | 2 | 8 |
| g6 -> g23.in[2] | - | 4 | 3 | 2 | 9 |
| g23 -> g24.in[0] | - | 4 | - | 2 | 6 |
| g24 -> g25.in[0] | - | 4 | - | 2 | 6 |
| g25 -> g40.in[0] | 3 | 4 | 6 | 2 | 15 |
| g40 -> g41.in[0] | - | 4 | - | 2 | 6 |
| g41 -> g44.in[1] | - | 4 | 1 | 3 | 8 |
| g44 -> g45.in[0] | - | 4 | - | 2 | 6 |

## Conclusion

**The hypothesis is not supported.** Left-edge track assignment does not
strand nets on far-away tracks in these circuits: column repeaters are 0-6%
of the total everywhere, and 0 on 3 of 4 measured critical paths (3 of 73
repeaters, 4%, even on the fourth, and split across one edge that also has
larger ramp and track contributions). If track placement were routinely
stranding nets far from their endpoints, column would show up as large and
volatile; instead it is small and mostly absent.

**What actually dominates is a fixed per-hop tax, not a variable one.**
Ramp is exactly 4 repeaters (8 game ticks) on literally every measured
critical-path edge, in every circuit, because `GATE_Y`/`TRACK_Y` are two
levels apart and every hop climbs and descends once -- 49-67% of
critical-path repeaters. Gate-entry's mandatory socket-terminating repeater
(plus a second one for the west/east corner turn most sockets need) adds
another 27-33%, equally independent of circuit size. Together these floor
every hop at 5-6 repeaters (10-12 game ticks) *before* a single block of
track or column is walked -- which is exactly why the ratio is already
7.25x on `and4`'s 7 gates and does not grow much faster than that as
circuits get bigger (7.25x, 7.20x, 8.22x, 9.22x across a 4x-to-20x range of
gate count): most of the routing tax does not scale with the netlist at all.

Track is the one part that does scale with circuit width, from ~4% of
critical-path repeaters on the two smallest circuits to 14-19% on the two
built from the seven-segment decoder's minterms (which have wide fan-in,
pulling gates and their consumers apart in X). That growth is real and worth
tracking as circuits scale further, but it is still smaller than the fixed
ramp+gate-entry floor even on the biggest circuit measured.

Confidence: high. The scanning methodology is validated two independent
ways (exact whole-world repeater match; exact settle-tick reconstruction
from critical-path repeater counts) across all four reference circuits, not
approximately -- both checks land on the exact measured numbers, with zero
slack to hide a misattribution in.

## What this does not do

- No placement or routing decision changed. `check_dims`-style verification
  (compiled bounding box and non-air count) confirms this:
  and4 66x5x83/791, full_adder 68x5x214/2911, segment_a 148x5x233/8112,
  seven_segment 232x5x298/19050 -- all unchanged from before this work.
- No fix is proposed or implied beyond what the numbers show. In particular,
  this does not claim ramp cost is easy to remove (`GATE_Y`/`TRACK_Y`
  separation exists for a real reason -- see `compile::mod`'s "Placement and
  routing" section on why tracks and columns need different layers) or that
  track growth is the next thing worth optimising. That choice belongs to
  whatever task reads this one.
