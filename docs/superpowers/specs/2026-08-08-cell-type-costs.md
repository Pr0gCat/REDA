# Pricing the cell types honestly

> **Status, 2026-08-09.** The cost measurements below stand — they are
> NOR-network constructions run through the real placer and the real simulator,
> and nothing about redstone has changed. What has changed is who reads them.
> `src/frontend/redstone_nor.genlib` was deleted in `7bb3155`; ABC is handed no
> library and does no mapping. The price list lives in `src/compile/topology.rs`
> now (`expansion_cost` / `entry_cost`), which `compile::lowering::lower`
> consults directly. So read "the genlib" throughout this document — and the
> whole "What this implies for the genlib" section — as "the topology library",
> and read every recommendation about what to offer ABC as moot: ABC is not
> being offered anything. `genlib_cost` survives as a function name only; its
> doc comment says so.
>
> Two other claims here have expired the same way. `Gate` is no longer "the
> NOR-only `Gate` type" with "no kind field at all" -- it has carried
> `kind: topology::GateKind` since `ac98e35`, which is what the referenced spec
> asked for. The gap this document identified around a comparator is still real;
> the reason given for it is not.
>
> The reference-circuit spot check under "Status" was true at the commit it was
> taken at. It is not current: `verilog seven_segment` is now 12348 blocks / 88
> ticks, measured, and `2026-08-09-polarity-assignment.md` explains why it grew.
> The four hand-written rows are unchanged.

## Status

Done: every cell type asked for is measured, two of the spec's non-NOR claims
are checked against the real simulator, and both turn out to be genuine
(delay/primitive wins) but blocked on infrastructure that does not exist yet.
**No source under `src/` changed.** The genlib stays as it is. A previously
unknown router edge case was found in passing and is documented, not fixed --
see "A discovered bug, not fixed here" below.

Measurement harness: `tests/cell_type_costs.rs` (new file, committed with
this report). Commit: see the commit that adds this file and its harness
together (`git log` on `feat/walking-skeleton` after this doc lands).

Tests: **291 passed, 0 failed, 0 ignored** (273 pre-existing + 18 new, all in
`tests/cell_type_costs.rs`). `cargo clippy --all-targets -- -D warnings`
clean. The five reference-circuit numbers this task was not allowed to move
are unchanged, spot-checked after the change:

```
and4            472 blocks   24 ticks
full_adder     1784 blocks   62 ticks
segment_a      6416 blocks   82 ticks
seven_segment 16244 blocks  112 ticks
verilog seven_segment 8130 blocks  70 ticks
```

This is expected -- nothing under `src/` was touched, only a new test file
was added -- but it was measured, not assumed.

## Method

For every cell type, two questions, both answered by building something and
running it through the real simulator rather than by estimating:

1. **What does it cost realised the way this compiler builds things today?**
   Built as a NOR network -- the smallest one this task's author could find
   by hand for each function -- run through the real placer/router
   (`compile`) and the real `Simulator`, exactly the way
   `tests/reference_circuits.rs` measures the project's reference circuits.
   Every construction's truth table is checked against the compiled,
   simulated circuit, not against the boolean algebra alone.
2. **Does a cheaper, non-NOR realisation exist?** Checked by hand-building a
   `World` directly -- bypassing `compile` and the NOR-only `Gate` type
   entirely -- and running it through the same real `Simulator`. Two of the
   spec's claims were checked this way (see "Non-NOR realisations" below);
   both hold.

All of this lives in one new file, `tests/cell_type_costs.rs`. Run
`cargo test --test cell_type_costs -- --nocapture` to see every number
printed, or `cargo test --test cell_type_costs cell_type_cost_table --
--nocapture` for just the summary table.

## The cell-type table

Yosys's `synth` (no `abc`) emitted seven cell types on the seven-segment
decoder: `$_OR_` (6), `$_NAND_` (9), `$_ANDNOT_` (6), `$_NOR_` (3), `$_AND_`
(5), `$_ORNOT_` (1), `$_MUX_` (1). `$_NOT_`, `$_XOR_`, `$_XNOR_` didn't appear
on that circuit but are standard Yosys output elsewhere, so they're priced
too, plus `BUF` (already priced in `redstone_nor.genlib`, included for a
complete picture).

| cell | NOR-network construction (gates) | gates | blocks | settle (game ticks) | blocks/gate |
|---|---|---:|---:|---:|---:|
| NOT | `NOR(a)` | 1 | 38 | 8 | 38.0 |
| BUF | `NOT(NOT(a))` | 2 | 78 | 11 | 39.0 |
| NOR2 | native | 1 | 84 | 10 | 84.0 |
| NOR3 | native (see caveat below) | 3* | 386* | 22* | 128.7* |
| AND | `NOR(NOT a, NOT b)` | 3 | 164 | 11 | 54.7 |
| OR | `NOT(NOR(a, b))` | 2 | 124 | 14 | 62.0 |
| NAND | `NOT(NOR(NOT a, NOT b))` | 4 | 196 | 18 | 49.0 |
| ANDNOT (`a & !b`) | `NOR(NOT a, b)` | 2 | 158 | 14 | 79.0 |
| ORNOT (`a \| !b`) | `NOT(NOR(a, NOT b))` | 3 | 214 | 20 | 71.3 |
| XOR | XNOR's 4 gates + 1 NOT | 5 | 414 | 28 | 82.8 |
| XNOR | `NOR(NOR(a,n1), NOR(b,n1))`, `n1=NOR(a,b)` | 4 | 374 | 22 | 93.5 |
| MUX (`s ? b : a`) | SOP: `OR(AND(!s,a), AND(s,b))` | 7 | 494 | 30 | 70.6 |

\* NOR3's bare form (one gate, three primary-input levers, nothing else)
**does not compile** -- see "A discovered bug, not fixed here". This row is
NOR3 plus one `BUF` (worked around by feeding the third input through a
harmless `BUF` instead of straight off its lever), so it overstates the bare
gate's cost by roughly one BUF's worth (2 gates, 78 blocks, 11 ticks,
measured separately above) -- not an exact subtraction, since routing
interacts with the rest of the layout, but a rough correction lands bare
NOR3 somewhere near 300 blocks, 1 gate, still cheap in gates even though the
number here isn't a clean per-gate figure.

Every gate-count column above is a **measured minimal-by-hand construction**,
not a proven-minimal one -- no exhaustive/SAT search for a smaller NOR
network was run. The pattern that emerges, though, is structural and worth
stating plainly because it should drive the genlib's shape later:

- **AND-shape outputs are cheap; OR-shape outputs cost one more gate.**
  `NOR(x,y) = !x & !y` computes an AND of two negations for free. Any
  function of the form `p & !q` costs only the one negation `q` didn't
  already have (`ANDNOT`: 2 gates) or `p & q`'s two negations if neither
  input arrives pre-complemented (`AND`, `NOR3`-fed-by-NOTs: 3 gates). An
  OR-shape output pays that same negation cost and *then* a final inversion
  on top, because NOR's native output is AND-shaped: `OR` (2, no internal
  negation needed), `ORNOT` (3, one internal negation), `NAND` (4, two).
  XOR/XNOR split the same way: XNOR (an "agreement" gate, same family as
  AND/NOR) is 4 gates; XOR needs XNOR's 4 plus one more inversion, 5.
- This means **the two things Yosys emits most on the real circuit --
  `$_OR_` and `$_NAND_`, 15 of the seven-segment decoder's 31 pre-mapping
  cells -- are exactly the shapes this NOR-network realisation prices
  highest relative to a same-arity AND-shape gate.** That is the 79%-of-gates-
  are-ORs finding from the referenced spec, restated at the single-gate
  level instead of the whole-circuit level.

## Non-NOR realisations

Two of the spec's claims were checked against the real simulator. Both hold.

### OR is a free wire merge -- when nothing else shares the branch

**Verified.** `or_is_a_free_wire_merge_when_nothing_else_shares_the_branch`
builds a straight run of five plain dust cells with a lever directly against
each end -- no torch, no repeater, no gate of any kind. Reading the middle
cell's own `power` field across all four input combinations confirms it is
nonzero exactly when either lever is on. Measured: **12 blocks, 0 gates, 0
game ticks to settle** (the merge is not merely cheap, it settles
instantaneously -- there is no active component anywhere in the circuit to
introduce delay). Compare the NOR-network `OR`'s row above: 2 gates, 124
blocks, 14 ticks. The free merge is not a small win, it is the whole cost.

**The backflow half of the claim is also verified, both ways.**
`or_merge_without_isolation_corrupts_a_branch_that_fans_out_elsewhere` builds
lever A driving a fork; one branch runs through a repeater into a support
block with its own standing torch (meant to read `NOT(A)` and nothing else);
the other branch is two more cells of plain dust running straight into an
unisolated merge with lever B. With A off and B on, the consumer torch goes
dark -- it should read `NOT(0) = lit`, but B's signal ran backward up the
shared dust and reached the repeater's input, corrupting it into reading
`NOT(A OR B)` instead. `or_merge_with_isolation_protects_a_branch_that_fans_out_elsewhere`
is the identical layout with one repeater inserted on the shared branch
(exactly where the referenced spec's fanout rule says to put it), and the
corruption disappears: the merge still correctly sees B (the isolating
repeater still forwards A's own signal into it), and the consumer torch is
unaffected by B either way. This is the fanout rule from
`docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md` confirmed by
construction, not just by argument.

**What blocks building this today**: exactly what that spec already says --
`verify_connectivity` assumes one source per net, so any wire-merge OR (bare
or isolated) is rejected before it ever reaches the simulator. This task
does not touch that invariant; it only confirms the physics claim the
invariant work was going to be justified by.

### ANDNOT is one comparator in subtract mode

**Verified**, and it was not expected going in that the simulator already
fully implements this: `redstone/simulator/component.rs`'s
`comparator_output` already has subtract-mode support
(`rear.saturating_sub(side)`), complete with the real game's side-input rule
(side inputs need **strong** block power, not a bare dust line -- verified
by using a lever standing directly on the side block, not dust resting on
top of it).

`andnot_is_one_comparator_in_subtract_mode` wires lever A to the comparator's
rear (main) input via a short dust run, lever B to strongly power one side
input directly, and reads the front output dust. All four input
combinations check out: `max(0, 15·A - 15·B)` is exactly `A & !B` for
boolean-strength inputs. Measured: **9 blocks, 1 primitive, worst-case
settle 2 game ticks** -- half the delay of the NOR-network's 2-torch chain
(2 torches in series is `2 x TORCH_DELAY_GAME_TICKS = 4` game ticks of pure
logic delay before routing; `COMPARATOR_DELAY_GAME_TICKS` is the same 2 as
one torch, but there is only one of it here).

**What blocks building this today** is bigger than OR's gap, and worth being
precise about so nobody mistakes this for a quick follow-up:

- `Gate` has no way to say "this is a comparator" -- it is (per the
  referenced spec) a NOR-only type with no kind field at all.
- There is no `place_comparator_gate` -- `place_nor_gate` is the only
  gate-placement primitive `compile` has, and a comparator's geometry (a
  rear input, two side inputs with a *different* powering rule than a
  repeater-terminated approach, and a front output) is not a NOR cell with
  more inputs, it is a different shape entirely.
- The router's net-termination logic (`lay_bent_path`, `Route`, the
  Ramps/Columns/Tracks passes) only knows how to terminate a route into a
  NOR support block via a mandatory final repeater. A comparator's side
  input specifically *rejects* that -- it needs a strongly-powered block,
  not a repeater-terminated dust run landing on a support -- so the router
  would need a second termination style, not just a new destination shape.
- `topology::Primitive::Comparator` already exists in the vocabulary (it was
  added "reserved for a future comparator-based entry," per that module's
  own doc comment) but has zero `Template` entries using it, and
  `genlib_cost` only knows how to price a `Torch` node (`assert_eq!` there
  would need a real branch added, not just relaxed).

So ANDNOT's cheaper realisation is real and the delay win is real, but it
needs a new placement capability, not an invariant relaxation -- more work
than OR's path, and arguably a separate task with its own design (what does
a `LibraryEntry` built from a `Comparator` node look like; how does the
router's termination logic branch on it) rather than a quick addition once
the OR invariant work lands.

### MUX: no cheaper realisation found

The spec flagged that "known redstone constructions" exist for MUX. This
task looked and did not find one that is both (a) a genuine improvement over
the 7-gate NOR-network SOP construction and (b) buildable without the same
comparator-class placement infrastructure gap ANDNOT hit, within the time
available. Community redstone designs for 2:1 multiplexers commonly lean on
pistons, clocked latches, or comparator-based signal selection -- all of
which either aren't purely combinational or need the same new placement
primitive work ANDNOT does. This is left as an open question rather than a
finding either way: MUX is priced here only via its NOR-network cost (7
gates, 494 blocks, 30 ticks).

## A discovered bug, not fixed here

`bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case` documents
a router edge case found while building the NOR3 measurement: a single NOR3
gate wired directly to three primary-input levers, with no other gate in the
netlist, fails `compile` with `ConnectivityViolation` (two input nets' routed
dust ends up electrically joined).

This is **not** a NOR3-arity problem in general -- every reference circuit
(`and4`, `full_adder`, `segment_a`, `seven_segment`) is full of NOR3 gates,
and none of them trips this, because `NetlistBuilder::and_reduce` always
interposes a `NOT` gate before feeding any multi-input NOR, and `or_reduce`
in this project is only ever called on already-derived signals in practice,
never on three raw top-level inputs at once. Isolating it (two probes tried
during this task, since folded into the test's own doc comment): a 3-input
NOR with *any one* of its three sockets fed by a gate output instead of a
lever compiles and simulates correctly, regardless of which socket that is
(West, East, or the South one an arity-<=2 gate never even uses). Only "all
three inputs are primary-input levers, and there is nothing else in the
netlist" reproduces the failure -- pointing at the primary-input row /
bypass-routing interaction specifically, not at `place_nor_gate`'s own
per-gate geometry.

Flagged as a follow-up task (out of scope here -- this task measures, it
does not change the compiler), and a background task suggestion was raised
for it separately.

**Fixed 2026-08-09.** `docs/superpowers/specs/2026-08-09-channel-safety-condition.md`
derived the real dust-adjacency safety condition and, in the course of that,
found the same "primary-input row / bypass-routing interaction" as a
two-*gate* netlist, traced it to `resolve_bypass_and_geometry`'s widened
bypass pass checking every candidate's horizontal jog against a `Reservation`
snapshotted once before its loop (so two candidates approved in the same pass
could never see each other's jogs), and fixed it by making that reservation
live. `bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case` (this
report's test) is now `..._now_compiles_and_matches_its_truth_table` and
measures the bare gate directly. NOR3 as a genlib-mapped target is no longer
blocked on this specific bug, though the broader question below (whether it
should be genlib-mapped at all yet) is unaffected.

## What this implies for the genlib

**Do not write the genlib yet for OR or ANDNOT.** Both have a real, verified,
cheaper realisation, and both are still blocked on infrastructure that does
not exist: OR on the multi-source-net invariant relaxation the referenced
spec already scoped (step 1 of its own order: invariant first, then the OR
gate kind and library entry, then isolated-merge entries, then the genlib
line); ANDNOT on a new comparator placement primitive that has not been
designed at all yet. Quoting either at its cheaper price before the frontend
can build it would be exactly the mistake this task's brief warned against:
a price list ABC would believe and this compiler could not honour.

**What could be priced honestly today, if this project wants to broaden the
genlib beyond NOR/BUF before the OR/ANDNOT work lands**: AND, NAND, ORNOT,
XOR, and XNOR all have real, measured NOR-network costs above, built the
same way `topology::Library`'s existing NOR1/NOR2/NOR3/BUF entries are (a
`Template` with real nodes and internal edges -- these would need multi-node
templates, since e.g. AND is 3 torches, not 1, unlike every entry the
library ships today). This would let ABC's mapper choose these shapes
directly instead of building them itself out of NOR2/NOR3 as it does now --
plausibly with better results, since ABC's own decomposition may not match
the specific minimal constructions measured here. Whether that is worth
doing before the OR work lands is a separate judgement call: it does not
reduce the 79%-of-gates-are-ORs problem the referenced spec exists to fix
(AND/NAND/ORNOT/XOR/XNOR were not the circuit's dominant cost; OR was), so it
would be a genlib-completeness improvement, not a fix for the actual
measured problem.

**NOR3 as a genlib-mapped target for a bare 3-input gate should wait on the
router bug above being fixed or at least understood better**, if any new
library entry could plausibly cause ABC to map a NOR3 directly onto three
undriven primary inputs with nothing in between (unlikely in a real
synthesised netlist, per the analysis above, but not impossible, and the
failure mode is a hard `ConnectivityViolation`, not a wrong answer -- better
to know the netlist that triggers it can't currently arise than to assume
so).

**Once OR's invariant work lands**, the expectation from the referenced spec
stands and this task's numbers sharpen it: ABC currently pays 124 blocks / 2
gates / 14 ticks for something the real hardware does in 12 blocks / 0 gates
/ 0 ticks whenever backflow is harmless (the common case, per the fanout
rule verified above), and something close to that plus one repeater's worth
of ticks when it is not. The two variants costing differently, as that spec
already flagged, is confirmed rather than merely anticipated: a bare merge
and an isolated one are not the same price, and the genlib (once it can
speak at all for OR) will need to say which one ABC is being offered, or
offer both as separate entries the way `Library` was always shaped to allow.
