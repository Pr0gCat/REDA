# Gate types the netlist can actually use, and the free OR

> **Status, 2026-08-09.** This landed, and then went further than it asked for.
> `Gate` gained its `kind` field and OR became a declared wire merge, as specced
> — but the genlib was not broadened, it was **deleted** (`7bb3155`). ABC is
> handed no library at all now; it optimises logic and stops at Yosys's default
> gate set, and `src/compile/lowering.rs` applies this project's own topology
> library to what comes out. So the code this document quotes below is gone:
> `src/frontend/redstone_nor.genlib`, the three-field `Gate` struct, its Chinese
> doc comment, and `realize_template`. Read the argument, not the snippets.
>
> The argument is preserved as written, including the parts the implementation
> overtook, because the reasoning is why the change happened. What is *not*
> preserved as-is: see `2026-08-09-polarity-assignment.md`, which measures what
> deleting the genlib cost — the synthesised decoder went from **0%** of the
> `NOR(n) -> NOT` pattern counted below to **50%** of it, because absorbing
> inversions across gate boundaries was ABC's other job and nothing took it
> over.
>
> The table below re-measures exactly today (`cargo run --release --bin mc_dump
> -- <circuit>`, counting a `NOR(n)` whose sole consumer is a 1-input NOR,
> together with that consumer): 8/22, 28/46, 66/84.

## The measurement that started this

Counting what the compiled decoder's gates are for:

| circuit | gates | of which are `NOR(n) -> NOT`, i.e. an OR |
|---|---|---|
| full_adder | 22 | 8 (36%) |
| segment_a | 46 | 28 (61%) |
| seven_segment | 84 | **66 (79%)** |

Four fifths of the decoder's logic is spent constructing ORs out of NORs.

**In redstone an OR is free.** Two dust lines joining is an OR — strength takes
the maximum, which is exactly the operation. It needs no gate at all.

## Why the compiler cannot see that

`src/frontend/redstone_nor.genlib` declares NOR1, NOR2, NOR3 and BUF. That is
the entire vocabulary ABC is given, so ABC can only build with NOR, and pays two
gates every time it needs an OR.

This is not ABC being wrong. **We handed it a price list that misreports the
cost of the cheapest operation in the technology**, and it optimised hard
against it. The project's founding observation was that redstone's cost model
is not CMOS's; we told ABC one half of that (fan-in barely affects delay) and
omitted the larger half.

The netlist being NOR-only is a consequence of the price list, not a fact about
redstone. **A gate type in the netlist and the primitives it is realised from
are different things**, and conflating them is what produced the 79%.

## The information is thrown away twice

It is worth being precise about where, because the fix follows from it.

```rust
pub struct Gate {
    pub name: String,
    pub inputs: Vec<String>,
    pub output: String,
}
```

`Gate` has **no kind field**. The type's own doc comment says so outright:
"只有 NOR 一種閘". So even a netlist that knew it contained an OR would have
nowhere to record it.

And upstream, `abc -genlib <NOR-only>` discards the AND/OR/XOR structure Yosys
had before mapping. The frontend does briefly hold a `GateKind` per Yosys cell —
and then `realize_template` flattens it into NOR `Gate`s.

So the higher-level structure exists in Yosys's output and is destroyed on the
way in, twice over.

The temptation is to recover it afterwards by pattern-matching NOR clusters
(`NOR(¬x…)` is an AND, `¬NOR(x…)` is an OR). That works, and it is the wrong
fix: it spends effort reconstructing what we chose to delete, and it can only
ever be as good as the patterns someone remembered to write.

**Stop deleting it instead.** `Gate` gains a kind; the genlib offers ABC a
vocabulary that includes the gates redstone is good at; the frontend keeps what
Yosys gives it. Then the topology library does the job it exists for — kind to
primitives — and there is nothing to detect.

## What changes

Gate types become what Yosys emits — AND, OR, XOR, NOR — and the topology
library says how each is realised:

| gate | realisation |
|---|---|
| NOR | one torch |
| OR | a wire merge |
| AND, XOR, ... | structures built from torches |

The genlib then declares each type's **redstone** cost, so ABC optimises
against the technology we actually have.

This is also the point at which the topology library stops being a formality.
NOR maps one-to-one onto a torch and carries no information; **OR maps onto no
primitive at all**, and only this layer can say so.

## An OR is a node, not a disappearing act

A merge could be modelled by collapsing the OR into its net — one fewer node,
tidier graph. That is wrong here.

Redstone dust is **bidirectional**. Joining two lines lets each source's signal
run backwards into the other's, which can power things upstream that should not
be powered. A real OR therefore often needs **diode isolation** — a repeater on
a branch so signal only flows into the merge.

So an OR is free only when backflow is harmless, and costs repeaters and ticks
when it is not. That judgement needs the placement context, so it belongs to the
planner — and the planner needs a node to attach it to. Collapsing the OR into a
net throws away the very thing the decision hangs on.

**OR is therefore the first gate type with genuinely alternative library
entries**: a bare merge, and merges isolated on some or all branches. Until now
"more than one entry per gate type" was a reservation with no occupant.

### When isolation is actually needed, and how to know

Backflow is harmless more often than it first looks. When two branches merge,
each source's signal does run back along the other's wire — but if that wire's
only destination was this merge, everything on it was going to carry the merged
signal anyway.

It becomes harmful when a branch's source **also feeds something else**. Then
that other consumer sees a signal it should not: the merge's output leaking
back up a wire it shares.

So the rule is decidable from the netlist alone:

> Isolate a branch when its source fans out to anything besides this merge.
> Otherwise a bare join is correct.

No placement information is required, which means the alternative entries are
selectable now rather than waiting on a planner. That does not make the planner
irrelevant to the choice — it may later find isolation worth adding for reasons
of strength budget or timing rather than correctness — but correctness does not
depend on it.

Verify this reasoning against the simulator before building on it. It is a
claim about redstone behaviour, and this project's record with claims about
redstone behaviour that were not checked is poor.

## The invariants have to allow multi-source nets, carefully

`verify_connectivity` assumes one source per net; `verify_torch_merge` assumes
nothing foreign reaches a support. A wire-merge OR is exactly a net with several
sources, so both currently forbid it.

**This is the dangerous part of the whole change.** A legitimate merge and the
bug we have hunted repeatedly — two unrelated nets' dust touching — are
*geometrically identical*. The only thing that separates them is whether the
netlist asked for the join.

So the relaxation must be exactly that: a net may have several sources **when
the netlist says it is a merge**, and not otherwise. An invariant that simply
permits multiple sources is not a weaker check, it is the removal of the check
that has caught the most bugs in this project.

The spec's earlier note stands: these are constraints, not cost terms, and
nothing may be weakened to make a layout fit.

## What to measure

Baseline at `e2ee43e`, all verified in a real 1.20.1 server:

| circuit | gates | blocks | settle | blocks/gate |
|---|---|---|---|---|
| and4 | 7 | 472 | 24 | 67.4 |
| full_adder | 22 | 1784 | 62 | 81.1 |
| segment_a | 46 | 6416 | 82 | 139.5 |
| seven_segment | 84 | 16244 | 112 | 193.4 |
| seven_segment (Verilog) | 37 | 8130 | 70 | 219.7 |

The Verilog decoder is the one that will move, because it is the only circuit
whose structure ABC chooses. Gate count is the first thing to watch: if the
price list is now honest, ABC should stop buying ORs at two gates each.

The hand-written circuits are built by `NetlistBuilder`, which constructs ORs
the same expensive way. Whether to teach it the cheap OR too, or leave it as the
control group that shows the difference, is worth deciding deliberately.

## Order

1. **Multi-source nets in the invariants**, gated on the netlist declaring the
   merge. Nothing else can land safely first, and it is independently testable:
   a hand-built world with a declared merge passes, the same geometry without
   the declaration fails.
2. **OR as a gate type and a library entry**, with the bare-merge realisation.
3. **Isolated-merge entries**, chosen by the fanout rule above. This is not
   deferred: the rule needs only the netlist, and without it a bare merge is
   wrong wherever a source is shared.
4. **The genlib gains OR** at its real cost, and the frontend maps `$_OR_` onto
   the entries. This is where the gate count should fall. Note that the two
   variants cost differently, so what the genlib should quote — and whether one
   number can honestly stand for both — is a real question, not a formality.

## Out of scope

- AND and XOR entries. OR is where the measured cost is; adding gate types for
  their own sake is not the point.
- The planner. This makes its job smaller, which is a good reason to do it
  first.
