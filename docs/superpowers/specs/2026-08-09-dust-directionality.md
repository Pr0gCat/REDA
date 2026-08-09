# Dust directionality: which *block* a wire powers

## Why this exists

`verilog:and4` spends a repeater to strong-power a gate's support block. That
is legal, and it is what the compiler does everywhere. What it exposed is that
the system had **no model of redstone dust's directionality at all** — and,
worse, a comment claiming otherwise. `src/redstone/rules/taxonomy.rs`, at
`0889c7e`:

```rust
// 紅石粉：**弱**充能腳下的方塊。水平方向的充能取決於粉的連接形狀，
// 由 `propagate` 依連接關係處理，這裡只回答垂直的部分。
BlockKind::RedstoneWire => match direction {
    Facing::Down => full,
    _ => PowerOutput::INERT,
},
```

*"The horizontal case depends on the dust's connection shape and is handled by
`propagate`."* It was not. `propagate::block_signal_at` reached blocks only
through `power_emitted_toward`, which returned `INERT` for every horizontal
direction; `simulator::connectivity` computed dust connections and fed them
only to dust-to-dust propagation. A dust run never energised the block it
pointed into, and a comment said the opposite. A second, independent copy of
the same false model sat in `compile::structural_output`
(`src/compile/mod.rs:4218`), where the torch-merge invariant depends on it.

The claim to be tested came in as a hypothesis, not a fact:

> Dust alone has no direction; a line of more than one dust does, taking its
> direction from the line, and the block it points into can be energised by it
> — but once the head or tail is joined by certain redstone blocks, it loses
> that direction.

Substantially right, and not precise enough to implement. So it was measured
first.

## The measured rule

Against a real 1.20.1 server over RCON, conformance category
`dust-directionality`, five probes / 27 checks, all passing:

| direction from the dust | powered? |
|---|---|
| **down** | always — the block a dust cell stands on, whatever its shape |
| **up** | never |
| **horizontal `D`** | only when the `D.opposite()` side is attached **and neither perpendicular side is** |

The horizontal row is one rule, not four cases, and every shape falls out of
it:

| shape | attached sides | powers, horizontally |
|---|---|---|
| straight run | `{E,W}` | the blocks at **both** ends of its own axis |
| one-sided stub | `{W}` → filled to `{E,W}` | same — see the fill rule below |
| corner | `{N,W}` | **nothing** |
| T | `{N,S,W}` | **nothing** |
| four-way cross | `{N,S,E,W}` | **nothing** |
| lone dot | `{}` | **nothing** |

Two things about it are not guessable and were both measured:

**The one-sided fill.** Vanilla completes a wire attached on one axis only
into a straight run. Without it, a run's *last* cell — which touches nothing
but the cell behind it — would have no axis and would power nothing, which is
the opposite of what the game does. Read live off the server: a lone dust cell
joined from one side reports `east=side,west=side`, and the lamp on its far
side lights.

**"Connected" is structural, not electrical.** A side counts as attached if it
carries dust (same layer, climbing, or descending) *or* if it holds a component
dust connects to. An **unpowered** repeater lying along the cross axis attaches,
and costs a perpendicular run its direction; the same repeater turned across
that axis does not attach and changes nothing. A climb side counts like any
other side.

The power is **weak**, always. It cannot re-drive dust — but it is enough to
light a lamp and, decisively, **enough to invert a torch attached to the
block**. That is the one that mattered: a torch reading its support is how
every gate in this compiler works.

### Probe transcript

`cd conformance && python run.py --properties ../minecraft-server/server/server.properties --out results/1.20.1.json --label 1.20.1`

```
[27/31] dust_shape_decides_which_block_it_powers ... OK
[28/31] a_climb_side_counts_as_a_connection_for_directionality ... OK
[29/31] a_component_beside_a_line_can_cost_it_its_direction ... OK
[30/31] dust_powers_the_block_below_it_but_never_the_block_above ... OK
[31/31] dust_weak_power_turns_off_a_torch_on_the_block_its_line_points_into ... OK
31 probes run, 2 disagreed with our code's assumption, 0 errored.
```

The two disagreements are pre-existing (`dust_climb_blocked_by_nonconductive_step`,
`conducts_glass` — the known glass case, where the code was fixed and the
probes' prose was not). No pre-existing probe's verdict moved.

The decisive checks, in full:

```
== dust_weak_power_turns_off_a_torch_on_the_block_its_line_points_into
    ok  baseline: straight lane's torch starts lit
    ok  baseline: branched lane's torch starts lit
    ok  straight: the run is powered
    ok  straight: the torch on the block it points into inverts off
    ok  straight: and its own dust goes dark with it
    ok  branched: the run is powered just the same
    ok  branched: the torch stays lit
    ok  branched: and its own dust stays at full strength
```

### The prediction that was wrong, and how it was caught

The rule above was written down *before* running anything, from a reading of
vanilla's `RedStoneWireBlock.getSignal`. One line of it was wrong: an isolated
dot was predicted to power **all four** horizontal neighbours (the "empty
connection set" branch). The server said it powers **none**.

"The lamp stayed dark" is also what a dead sensor looks like, so that was not
taken at face value. The dot lane now carries its own positive control, run in
place on the live server:

```
dust power=15                 : True
dust is a DOT (all four none) : True
   north=none  south=none  east=none  west=none
lamp lit                      : False
--- after joining a dust to the west (same cell becomes a straight run) ---
   north=none  south=none  east=side  west=side
dust power=15                 : True
lamp lit                      : True
```

Same cell, same lamp, same power, only the shape changed — and the lamp lit.
The sensor was alive; the prediction was wrong. A torch, in a second slot, gave
the same answer: a dot leaves it lit.

This is the good outcome of the rule that nothing about redstone gets asserted
from memory in this project. The measurement is also *simpler* than the
prediction: there is no special case for the dot at all, because a dot has no
attached side and so fails the horizontal condition like every other bend does.

## What the simulator does now

Three functions, one rule:

- `taxonomy::accepts_dust_connection(state, direction)` — is this side
  structurally attached? A repeater only along its own axis, an observer only
  on its output face, a comparator on all four (it has side inputs).
- `connectivity::dust_sides(world, pos)` → `DustSides` — the four attached
  sides, after the one-sided fill. Built on `dust_connections`, so there stays
  exactly one place in the crate that decides when dust reaches over a step.
- `connectivity::dust_powers_block_toward(world, pos, direction)` — the table
  above, **geometry only**.
- `propagate::dust_power_toward(world, pos, direction)` — the same geometry
  gated on the wire actually carrying a signal; consulted by
  `block_signal_at`, which is the only place a block learns it is powered.

`power_emitted_toward`'s dust arm still answers `INERT` horizontally, and now
says why: a `BlockState` alone genuinely cannot know its own shape. The false
comment is gone.

Two things deliberately did **not** change:

- **`signal_from`'s dust path.** A repeater or comparator reads the raw `power`
  of dust touching it, whatever the dust's shape — vanilla's
  `DiodeBlock::getInputSignal` has an explicit fallback for exactly this. A
  dust corner *does* drive a repeater it turns into; it just does not power the
  *block* beside it. Making that directional would have been a real bug.
- **`recompute_dust_strengths`.** It consults only *strong* block power, and
  this rule only ever adds weak power, so no dust cell's strength can move
  because of it. That is checked, not asserted
  (`no_dust_strength_can_move_because_of_the_directionality_rule`), because it
  is the reason no compiled circuit's settle time could move.

## The three questions

### 1. Is any currently-compiled circuit wrong in the real game? No — and not by luck.

Counted over every compiled world, with a positive control proving the query
detects the thing (a synthetic straight-run-into-a-torch world reports one
torch flip; the same world bent reports none):

| circuit | dust cells | dust beside a conductor | **pointing into one** | **torches that would flip** |
|---|---|---|---|---|
| `and4` | 208 | 12 | **0** | **0** |
| `full_adder` | 788 | 94 | **0** | **0** |
| `segment_a` | 2880 | 194 | **0** | **0** |
| `seven_segment` | 7360 | 335 | **0** | **0** |
| `verilog:and4` | 210 | 8 | **0** | **0** |
| `verilog:seven_segment` | 5636 | 366 | **0** | **0** |

Zero is not luck, and the middle column is why. Every one of those 1009 dust
cells that sits beside a conductive block has **exactly two attached sides**,
and in every case the conductor is *perpendicular* to the run — it is the stone
`seal_cross_talk` puts down to flank a channel, not something the run points
at. The compiler never terminates a run by butting it into a block: every one
of the 207 gate supports in these six circuits is fed by a **repeater**, and
not one by dust.

So the answer is structural, but it is a property of the current cell library,
not of anything enforced. Nothing stopped a future router from ending a run
against a block — which is what question 2 is about.

### 2. Is a spacing invariant under-strict? Not the one expected. The torch-merge one was.

The keep-out derivation from `dust_reach` is **sound as far as it goes** and is
not the gap. `dust_reach` answers "which cells, if they held dust, would join
this net", and `COLUMN_CLEARANCE = 2` is exactly necessary and sufficient for
that (`2026-08-09-channel-safety-condition.md`). Dust energising a *block* is a
different adjacency, but it is also not a *connectivity* question: a weakly
powered block cannot re-drive dust, so two nets can never merge through one.
Spacing and connectivity stay dust-reaches-dust, correctly.

The invariant that owns this adjacency is **torch merge**, and it was
under-strict. Its condition 3 — "`net_reach` says the support block is reached
by exactly the nets feeding this gate's declared inputs, no more" — is the only
check in the compiler that would catch a foreign net energising a gate's
support. And `net_reach` was built on `structural_output`, whose dust arm said
weak power goes *downward only*. A foreign run ending against a gate's support
block would have powered it in the real game and been invisible to
`ForeignNetReachesSupport`.

`net_reach` now asks `dust_powers_block_toward` for dust, in its geometry-only
form so the walk keeps its refusal to trust the freshly emitted world's
placeholder `power` fields. The invariant is strictly **stricter** than before:
it can now report a foreign net it previously could not see. It does not fire
on any of the six circuits — consistent with the census above, and the second
independent confirmation of it.

### 3. The missed optimisation: one repeater per gate.

Every gate support in every circuit is powered by a repeater whose only job is
to be a strong source. A dust run pointing into that block would do the same
job for free.

| circuit | gates | repeaters spent only on a support | total repeaters | critical path | settle |
|---|---|---|---|---|---|
| `and4` | 7 | **7** | 17 | 4 gates + 6 repeaters | 24 |
| `full_adder` | 22 | **22** | 79 | 10 gates + 19 repeaters | 62 |
| `segment_a` | 46 | **46** | 278 | 9 gates + 30 repeaters | 82 |
| `seven_segment` | 84 | **84** | 674 | 7 gates + 47 repeaters | 112 |
| `verilog:and4` | 9 | **9** | 17 | 4 gates + 8 repeaters | 28 |
| `verilog:seven_segment` | 39 | **39** | 495 | 5 gates + 37 repeaters | 88 |

**207 repeaters across the six circuits**, one per gate, exist only to strong-power
a block. The settle model is `2 × (gates + repeaters) + lamp`, so removing the
terminal repeater on each gate along the critical path is worth 2 game ticks
each — an *upper bound* of:

| circuit | settle now | floor if every terminal repeater goes |
|---|---|---|
| `and4` | 24 | 16 (−33%) |
| `full_adder` | 62 | 42 (−32%) |
| `segment_a` | 82 | 64 (−22%) |
| `seven_segment` | 112 | 98 (−13%) |
| `verilog:and4` | 28 | 20 (−29%) |
| `verilog:seven_segment` | 88 | 78 (−11%) |

**It is an upper bound, and it is not free.** Two costs have to be priced
before any of it is real:

1. **Signal budget.** A repeater refreshes to 15. A run that reaches the block
   itself must arrive with strength ≥ 1, so `verify_signal_strength` decides
   which of the 207 are actually removable, not this count.
2. **The firewall.** A repeater is why required clearance across its non-facing
   sides is **0** rather than 2 — `verify_connectivity`'s BFS only walks dust.
   Replace it with dust and the gate's input inherits the full keep-out, which
   costs area exactly where the layout is tightest. This trade has to be priced,
   not assumed.

Not implemented here, deliberately.

## Order of work

1. ~~Measure the rule against a real server.~~ Done — 5 probes, checked in.
2. ~~Correct the false comment.~~ Done.
3. ~~Implement the rule in the simulator, and in the one invariant that owns
   the adjacency.~~ Done. No circuit's geometry, size or settle moved: all six
   `build_circuit` outputs and all six settle figures are byte-identical to
   `0889c7e`, diffed against a worktree.
4. **Next:** price the terminal-repeater elimination against the signal budget
   and the clearance it costs. That is a router change and a separate task.

## Out of scope

- **The optimisation itself.** Quantified above, not implemented.
- **Vanilla's shape *history*-dependence.** A wire with no attached side renders
  as a dot if it was placed that way and as a four-way cross if it once had
  connections and lost them. `dust_sides` reports `NONE` for both, because a dot
  and a cross power exactly the same set of blocks — nothing horizontally — so
  the distinction is unobservable through this rule. If anything ever comes to
  depend on the rendered shape, this is where it breaks.
- **The descend gate.** `dust_connections` gates descending on the neighbour's
  ability to *support* dust; vanilla gates it on the neighbour's *conductivity*.
  They differ exactly on glass. Unmeasured, pre-existing, and untouched here —
  `dust_side_connected` deliberately reuses `dust_connections` rather than
  introducing a second, conflicting notion of what "connected" means.
- **26.2.** Every probe here relies on dust-mediated transitions, which
  `harness.py` records as unreliable on 26.2. Re-measuring there needs
  `/tick freeze` + `/tick step`, which nothing in the suite uses yet.
