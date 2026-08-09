# Global polarity assignment, and the inversions lowering cannot see

## Why this exists

`7bb3155` took technology mapping away from ABC. That was right — a mapper
priced in a genlib it was handed cannot choose a redstone realisation, and
choosing one is what `compile::topology` exists for. But ABC was doing a second
job inside the mapping, silently, and nothing took that one over.

The result is a measured regression. The Verilog seven-segment decoder is the
only circuit in this project whose structure is chosen by a tool rather than by
hand, so it is the only one that moved:

| `verilog:seven_segment` | gates | blocks | settle |
|---|---|---|---|
| `99107f4`, ABC technology-mapped | 31 | 7888 | 82 |
| `f70ef0e`, gate level + `lowering::lower` | 56 | 12348 | 88 |

Both rows measured in this session, the first from a worktree at `99107f4` (the
last commit that still had `redstone_nor.genlib`):

```sh
git worktree add /tmp/reda-old 99107f4
(cd /tmp/reda-old && cargo run --release --bin build_circuit -- verilog:seven_segment)
cargo test --release --test verilog_frontend -- --nocapture
```

**Beware the number 31.** It is the gate count of the *old, mapped* netlist —
NOR gates and merges, the things that get placed — and it is also, by
coincidence, the cell count of the *new, gate-level* netlist, which places
nothing at all until `lower` has run over it. Comparing 31 cells against 31
gates is comparing a circuit to a shopping list. The honest comparison is the
one above: 31 realisable gates then, 56 realisable gates now, for the same
boolean function.

Two older numbers for "the Verilog decoder" are still in `docs/`, both correctly
anchored to the commits they were taken at, and neither is a baseline for this
work: **40 / 8922 / 85** at `02110dd`
(`2026-08-08-3d-codesign.md`) and **37 / 8130 / 70** at `e2ee43e`
(`2026-08-08-gate-types-and-wired-or.md`, and quoted onward from there). This
project has already had one agent report a regression as a win by measuring
against the wrong one of these. **If a number here surprises you, re-measure it
before arguing from it**, and say which commit you measured at.

## What ABC was actually doing

Dump both netlists and the difference is not subtle:

```sh
cargo run --release --bin mc_dump -- verilog:seven_segment   # GATE lines
```

| | `99107f4` (ABC-mapped) | `f70ef0e` (lowered) |
|---|---|---|
| gates | 31 | 56 |
| 1-input NOR (inverters) | 6 (19%) | 23 (41%) |
| `NOR(n)` whose only consumer is a `NOT` | **0** | **14 pairs, 28 gates (50%)** |
| fan-in 3 used | `nor3` ×3, `merge3` ×5 | none |

The middle row is the whole story. `NOR(n) -> NOT` is an OR built out of NOR:
one gate to compute the inverted thing, one more to un-invert it.
`2026-08-08-gate-types-and-wired-or.md` opened by counting that pattern in the
hand-written circuits and finding four fifths of the decoder spent on it. Its
table re-measures exactly today — `full_adder` 8/22 (36%), `segment_a` 28/46
(61%), `seven_segment` **66/84 (79%)**, counting a `NOR(n)` whose sole consumer
is a 1-input NOR, together with that consumer.

That spec's fix landed: OR is a wire merge now, free, and the frontend keeps the
gate kinds. And the synthesised decoder, which is the circuit the fix was for,
has gone from **0%** of that pattern to **50%**.

The reason is not that the merge is missing. It is there — 17 of the 56 gates
are merges. The reason is that every expansion in `topology::expansion_for` is
written for one gate in isolation, in one fixed polarity, and inversions that
two neighbouring expansions each introduce have no pass that can see both.
`GateKind::And` expands to `NOR(!a, !b)`; `GateKind::Nand` expands to
`merge(!a, !b)`. Feed an `and` into a `nand` and the `and`'s output torch is
immediately inverted again by the `nand`'s input inverter, and nothing looks at
the pair.

That is what ABC was doing for free: it optimises over an AIG — a graph of
2-input ANDs where inversion is a *bit on an edge*, not a node — so pushing a
complement across a gate boundary costs nothing and is done globally, before any
gate exists to pay for it.

## Do not reach for the local fix

The obvious pass is to walk the lowered netlist and delete `NOT(NOT(x))`.
Measured, in the lowered decoder:

```
cancellable NOR1 -> NOR1 pairs: 0
```

Zero. Not "few" — none, and none in the old mapped netlist either. Double
inversion never appears as two adjacent torches, because `lower` routes every
intermediate inverter through `NetlistBuilder::not`, whose cache already shares
them. The waste is not two inverters in a row; it is one inverter standing
between two cells that could each have been built in the other polarity.

The measurement that says where it really is: of the 23 inverters in the lowered
decoder, **19 invert another cell's output** — only 4 invert a primary input.
And **14 of the 31 cells have every one of their consumers be an inverter**:
their true polarity is computed, routed, and then never read by anything.

Those 14 are, gate for gate, the same 14 the `NOR(n) -> NOT` count above finds
(`g0 g8 g9 g10 g11 g12 g16 g19 g20 g22 g23 g24 g26 g29`). That is not two
findings, it is one seen from both ends: "an OR built out of NOR" and "a cell
whose output polarity nobody wanted" are the same torch.

## The argument

**Polarity is a global property of a net, and this compiler currently decides it
per gate.** Every gate-level cell has two realisations, one for each output
polarity, and `topology::expansion_for` knows only one of them. Choosing them
independently is what puts a torch between `g0` and its only consumer; choosing
them together is what ABC's AIG did and what we removed without replacing.

So the pass this project needs is not peephole cleanup on the lowered netlist.
It is a decision made **before** lowering, on the gate-level netlist, that
assigns each net a polarity and hands `lower` a per-gate output polarity to
build for. Lowering stays a mechanical application of recipes — it must, or the
topology library stops being the single place a redstone realisation is chosen —
and the recipe table grows a complemented entry per kind, priced by
`expansion_cost` the same way every entry already is.

The cost model to optimise against is the one this project already has and ABC
never did. `expansion_cost` prices a complemented `and` (which is `nand`:
two inverters and a **merge**, no torch) against the plain `and` (two inverters
and a torch) in redstone terms, including the fact that the merge costs no torch
delay. That asymmetry is real, it is measured, and it is invisible to a mapper
working from a genlib.

## What must not regress

Measured in this session with
`cargo test --release --test reference_circuits -- --nocapture` and
`cargo test --release --test verilog_frontend -- --nocapture`:

| circuit | gates | blocks | settle | blocks/gate |
|---|---|---|---|---|
| and4 | 7 | 472 | 24 | 67.4 |
| full_adder | 22 | 1784 | 62 | 81.1 |
| segment_a | 46 | 6416 | 82 | 139.5 |
| seven_segment | 84 | 16244 | 112 | 193.4 |
| seven_segment (Verilog) | 56 | 12348 | 88 | 220.5 |
| and4 (Verilog) | 9 | 480 | 28 | 53.3 |

The four hand-written circuits are the control group and **must not move at
all**: `lower` is the identity on them, so any change to them means a polarity
pass has started rewriting netlists that were already realisable, which is out of
its remit.

The two Verilog rows are the ones this work exists to move, and the bar is
`99107f4`'s **31 gates / 7888 blocks / 82 ticks**. Beating the old mapped netlist
is the result; matching it is the minimum; anything above 56 is a second
regression on top of the first.

The four invariants stay, all four, unweakened: spacing, connectivity, torch
merge, signal strength. A polarity assignment that produces a layout one of them
rejects has produced a wrong circuit, not an expensive one.

Truth tables and the RCON conformance run against a real 1.20.1 server are the
floor, for both Verilog circuits. A smaller decoder that lights the wrong
segment is not a result. And `the_baked_seven_segment_is_a_gate_level_circuit_not_a_wall_of_nors`
pins the gate-level histogram exactly: if this work changes what arrives from
Yosys rather than what we do with it, that test says so.

## Order

1. **A complemented entry per `GateKind`, in `expansion_for`, priced.** Purely
   additive and independently testable: `expansion_cost` of each new entry,
   and a truth-table check that the complemented expansion really computes the
   complement. Nothing consumes it yet, and nothing moves.
2. **`lower` takes a per-gate output polarity**, defaulting to the polarity it
   builds today. The identity property on hand-written circuits and the
   idempotence property both have to survive this, and both already have tests.
   Still nothing moves.
3. **The assignment itself**, over the gate-level netlist, minimising
   `expansion_cost` across the whole graph rather than per gate. This is the
   step with a real algorithm in it and the one most likely to need more than
   one attempt.
4. **Fan-in packing**, separately measured. ABC's netlist used `nor3` and
   `merge3`; ours uses neither, and `place_nor_gate` accepts three inputs.
   This is a different missing optimisation that the same measurement exposed,
   and it should land with its own number rather than being folded into
   polarity's.

Each step lands with its own measurement against the table above. A step that
does not pay for itself is reverted rather than carried.

## Task 4 measured outcome: packing not retained

Task 4 tested the smallest boolean-proven pack: flatten a private selected
two-input wire-merge final stage into its only selected two-input OR or NOR
final-stage consumer, producing `Or(3)` or `Nor(3)` only when the combined
fan-in is exactly three. The selector rejected shared producers, declared
outputs, non-final-stage uses, directly realisable boundaries, and nested NOR
producers. Unit truth tables passed, but the complete routed decoder is the
physical authority.

At base commit `afb3a5f`, all eight subsets of the three deterministic decoder
candidates were measured with the release `verilog_frontend` acceptance test:

| candidate mask | gates | blocks | settle |
|---:|---:|---:|---:|
| 0 (Task 3, no pack) | 47 | 10,088 | 86 |
| 1 | 46 | 10,796 | 92 |
| 2 | 46 | 10,716 | 94 |
| 3 | 45 | 10,736 | 92 |
| 4 | 46 | 10,168 | 84 |
| 5 | 45 | 10,734 | 76 |
| 6 | 45 | 10,592 | 90 |
| 7 (all three) | 44 | 10,398 | 88 |

No non-zero subset is Pareto-better than Task 3: every one increases block
count, and most also increase settle time. Static cell-footprint savings did
not predict the routed result. Per the rule above, all packing code and tests
were removed rather than carrying a physical regression. Task 4 therefore
keeps the Task 3 decoder exactly at **47 gates / 10,088 blocks / 86 game
ticks**, with histogram `nor1:14 nor2:16 merge2:17`, and makes no planner,
router, viewer, or invariant change. A three-input pack can be reconsidered
only together with a physical placement model that proves it pays.

The retained 47-gate decoder was then verified against a newly started
vanilla Minecraft 1.20.1 server, not a previous report. The server jar SHA-1
was `84194A2F286EF7C14ED7CE0090DBA59902951553`; its new log identified version
1.20.1. `circuit_conformance.py` used the fresh origin
`170000,151,140000`, with its normal pre-build clear and
`verify_region_is_air` check enabled, placed all **10,088** non-air blocks,
and checked every output segment for all 16 input vectors. Result:
**16/16 vectors passed, 0 mismatches**. The harness then cleared the region,
released its forceload, and the server was stopped over RCON with both ports
confirmed closed. The complete per-vector evidence is committed as
`conformance/results/verilog_decoder_task4_afb3a5f.json`.

## Out of scope

- **Reconstructing gate kinds by pattern-matching NOR clusters.** That is the
  fix `2026-08-08-gate-types-and-wired-or.md` rejected, for the reason it gave:
  it spends effort recovering what we chose to delete. The kinds arrive intact
  now; the problem is downstream of them.
- **Putting the genlib back.** ABC absorbing inversions is the capability worth
  recovering, not the mechanism. A genlib is a price list handed to a tool that
  cannot see the geometry, which is the mistake this project already made once.
- **An e-graph.** `2026-08-05-redstone-eda-design.md` §5.3 wants tech mapping
  folded into e-graph extraction. Polarity assignment is a much smaller, much
  better-defined problem, and doing it first tells us whether the general
  machinery is needed at all.
- **The placer.** This makes its input smaller, which is a good reason to do it
  first.
- **Sequential logic.** `2026-08-08-sequential-logic.md` is a separate front;
  polarity across a flip-flop boundary is a question for whoever lands that.
