# Every coupling mechanism, measured

**Generated. Do not edit by hand.** Every mark below is a `Simulator` run, not a reading of the rules. Regenerate with

```
cargo test --release --test coupling_mechanisms -- --ignored regenerate_the_coupling_tables
```

and `the_committed_table_is_what_the_simulator_says_today` in the same file fails if this text and the simulator ever disagree.

Method, in full, lives in `tests/coupling_mechanisms.rs`'s module doc comment. In short: an emitter, an optional mediator block, and a receiver — usually a **bare dust cell with air beneath it**, so the mediator the row names is the only block that can re-drive it. The world is run to stable, the receiver is read, and the whole thing is repeated with the emitter cell written as air, as a control. A coupling is a reading that *changed*; a control that was not already quiescent is reported `~` and is never counted as one.

The dust-to-dust relation is **not** re-derived here. It already has its own artifact at `docs/derived/dust-join-relation.md`, measured the same way.

## Summary, computed from the runs below

* **60 direct-drive couplings into a dust cell** (Table 1), of which **57 are invisible** to `verify_connectivity`'s walk.
* **31 couplings into a dust cell across one stone block** (Table 2), of which **31 are invisible** to that walk — all of them. That is not a coincidence and not a bug in the walk's implementation: the walk follows `dust_connections`, and `dust_connections` has no edge that leaves a dust cell for anything but another dust cell, so no block-mediated coupling can ever appear in it.
* The emitters that reach a dust cell **through** a stone block, measured rather than read off the taxonomy: ["repeater", "comparator", "torch", "wall_torch", "lever", "button", "pressure_plate"].
* A lit lever directly below the mediator drives this many of the mediator's five other faces, by material: [("air", 0), ("stone", 5), ("glass", 0), ("lamp", 5), ("dust", 4)]. `air` is the negative control (nothing in the middle, nothing carried); `glass` is the full cube that does **not** conduct, which is the property `block_signal_at` gates on.
* A **weakly** powered block reaches these receivers and no others: ["wall_torch", "repeater", "comparator"]. A dust cell is not among them, which is why no amount of dust probing can find this class.
* `run_until_stable` **refuses** any world containing these emitters outright, with `SimulationError::UnsupportedComponent`: ["button", "pressure_plate"]. Their rows are taken from `Simulator::new`'s constructor-time `recompute_dust_strengths` instead, and are tagged `load-only`.
* And the walk does not even cover mechanism 1 completely. **1 of Table 4's 18 dust-against-dust rigs couple electrically and still land in two different components** of `verify_connectivity`'s own walk, as `(floor, mid, drove_the_upper_wire)`: [("air", "air", true)]. The cause is seed order against a one-way edge; Table 4 says how, and what is not known about whether it is reachable.

## The mechanisms, named

Each is an edge in the realised world's electrical graph. The netlist asks for the first two; realisation supplies the rest for free.

1. **dust ↔ dust.** `connectivity::dust_connections`. Derived in full at `docs/derived/dust-join-relation.md`. Same layer unconditionally, plus a gated climb and descend.
2. **component → adjacent dust.** Table 1. A lit lever, torch, redstone block, or a diode's output face lights a dust cell touching it with no block in between. Note the torch: it drives dust on five faces, withholding only its own support.
3. **component → block → dust.** Tables 2 and 3. Requires two things at once: the block conducts (`taxonomy::flags_of(..).is_conductive()`, the gate at the top of `propagate::block_signal_at`) **and** the power arriving is `BlockPower::Strong`. A block powered this way then drives dust on **every one of its six faces**, not only the face pointing away from the source. **Both shipped bugs are this mechanism.**
4. **component → block → torch / diode rear, on *weak* power.** Table 5. `component::torch_should_be_lit` puts a torch out when its support is powered *at all* (`block_power_at != None`), and `propagate::diode_rear_signal` hands a repeater or comparator the weak strength its rear block carries. So a wire whose run merely ends against a gate's support block turns that gate off, with no strong power anywhere and no dust-to-dust edge anywhere. This is the class `TorchMergeFailure::ForeignNetReachesSupport` names.
5. **weak power → dust.** Does not exist: `recompute_dust_strengths` seeds a wire from a neighbouring block only when `block_signal_at` answers `Strong`. Measured in Table 3b's `dust` rows and asserted by `a_block_powered_only_by_dust_drives_no_further_dust`.
6. **block → block.** Does not exist. `block_signal_at` reads its six neighbours through `dust_power_toward`, which defers to `power_emitted_toward` for everything that is not dust, and no arm there emits anything for a plain block. Measured by `a_strongly_powered_block_cannot_power_the_next_block`, which is also why a lamp sitting on a strongly powered block stays dark (Table 5, `across` × `lamp`).
7. **torch → its own support.** Does not exist, and that is the whole reason a torch inverts. The withheld direction moves with `facing`, measured at all four in `a_torch_never_powers_its_own_support`.
8. **quasi-connectivity.** **NOT MEASURED, and not modelled at all.** `src/redstone/simulator/mod.rs`'s module doc names it as out of scope. Nothing in this file can say whether a realised world contains one, because the simulator this file drives has no such edge to find.

## What `verify_connectivity` walks

Mechanism 1, and nothing else — and not all of that. `verify_connectivity` (`src/compile/mod.rs:5174`) seeds from every `BlockKind::RedstoneWire` cell and follows `dust_connections`; every other mechanism above leaves a dust cell for a block, or never touches a dust cell at all, and is therefore outside the relation it walks. **Mechanisms 2, 3 and 4 are entirely unchecked by it**, and mechanism 1 is checked except where a one-way edge runs against its seed order (Table 4).

Mechanism 3 is partly covered elsewhere — `verify_torch_merge`'s `net_reach` (`src/compile/mod.rs:5405`) does follow block power, in all six directions, and its `mark_powered` even carries the weak/strong split mechanism 4 turns on. But it is anchored at a **gate's own support block**: it asks which nets reach *this torch*, not whether any two nets became one somewhere else in the world. A lever over a route, or a torch under one, that lands nowhere near a gate's support is outside both invariants. That is the shape of the two shipped bugs, and it is why they shipped.

## Table 1 — the emitter against a bare dust cell on each of its six faces

No mediator at all: the receiver is a floating dust cell one step from the emitter, in the direction the column names. This is the direct drive relation — which neighbours a component lights with no block in between. `facing` is the component's own, and also the side its feed sits on, which is why the diodes read `x` in exactly one column: a diode's rear is where its input has to come from, so this rig cannot ask what a diode does to its own rear.

`J` coupled and invisible to `verify_connectivity`'s walk · `j` coupled and visible to it · `.` not coupled · `~` contaminated (the control was not quiescent) · `0` rig dead (the emitter never turned on) · `x` rig invalid (the feed would occupy or touch the cell under test) · `!` diverged.

```
emitter          facing  N S E W U D  settle
stone            -       . . . . . .  stable
glass            -       . . . . . .  stable
lamp             -       . . . . . .  stable
redstone_block   -       J J J J J J  stable
dust             -       x j j j . .  stable
repeater         N       x J . . . .  stable
repeater         S       J x . . . .  stable
repeater         E       . . x J . .  stable
repeater         W       . . J x . .  stable
comparator       N       x J . . . .  stable
comparator       S       J x . . . .  stable
comparator       E       . . x J . .  stable
comparator       W       . . J x . .  stable
torch            -       J J J J J .  stable
wall_torch       N       J . J J J J  stable
wall_torch       S       . J J J J J  stable
wall_torch       E       J J J . J J  stable
wall_torch       W       J J . J J J  stable
lever            -       J J J J J J  stable
button           -       J J J J J J  load-only
pressure_plate   -       J J J J J J  load-only
```

## Table 2 — the emitter across one stone block

Same sweep with a stone block inserted between: emitter at the origin, stone one step out in the column's direction, dust one step beyond that. A mark here is the shipped bugs' own mechanism — conductor, strongly powered block, foreign dust — and the emitter and the receiver are two cells apart, so no dust-to-dust edge can exist between them at all.

Read this against Table 1 and the difference is the whole weak/strong distinction: a torch drives dust on five of its six faces directly, and drives dust *across a block* on exactly one, because only its upward power is `BlockPower::Strong`.

```
emitter          facing  N S E W U D  settle
stone            -       . . . . . .  stable
glass            -       . . . . . .  stable
lamp             -       . . . . . .  stable
redstone_block   -       . . . . . .  stable
dust             -       x . . . . .  stable
repeater         N       x J . . . .  stable
repeater         S       J x . . . .  stable
repeater         E       . . x J . .  stable
repeater         W       . . J x . .  stable
comparator       N       x J . . . .  stable
comparator       S       J x . . . .  stable
comparator       E       . . x J . .  stable
comparator       W       . . J x . .  stable
torch            -       . . . . J .  stable
wall_torch       N       . . . . J .  stable
wall_torch       S       . . . . J .  stable
wall_torch       E       . . . . J .  stable
wall_torch       W       . . . . J .  stable
lever            -       J J J J J J  stable
button           -       J J J J J J  load-only
pressure_plate   -       J J J J J J  load-only
```

## Table 3 — the mediator's material, and which of its faces drive

The geometry of both shipped bugs: the driver sits **directly below** the mediator and the receiver is a bare dust cell on one of the mediator's five other faces. Sweeping the material answers what makes a block able to carry a coupling at all; sweeping the face answers which cells it then drives. Same alphabet as Table 1.

The two diodes read `x` in the `N` column: their feed has to sit at their rear, a diode's rear is horizontal, and that puts the feed one step from the mediator's own north face. A wire has no such constraint and is fed from directly below instead, so its five columns all survive.

```
driver           mediator  N S E W U  settle
lever            air       . . . . .  stable
lever            stone     J J J J J  stable
lever            glass     . . . . .  stable
lever            lamp      J J J J J  stable
lever            dust      J J J J .  stable
torch            air       . . . . .  stable
torch            stone     J J J J J  stable
torch            glass     . . . . .  stable
torch            lamp      J J J J J  stable
torch            dust      J J J J .  stable
wall_torch       air       . . . . .  stable
wall_torch       stone     J J J J J  stable
wall_torch       glass     . . . . .  stable
wall_torch       lamp      J J J J J  stable
wall_torch       dust      J J J J .  stable
repeater         air       x . . . .  stable
repeater         stone     x . . . .  stable
repeater         glass     x . . . .  stable
repeater         lamp      x . . . .  stable
repeater         dust      x . . . .  stable
comparator       air       x . . . .  stable
comparator       stone     x . . . .  stable
comparator       glass     x . . . .  stable
comparator       lamp      x . . . .  stable
comparator       dust      x . . . .  stable
dust             air       . . . . .  stable
dust             stone     . . . . .  stable
dust             glass     . . . . .  stable
dust             lamp      . . . . .  stable
dust             dust      . . . . .  stable
redstone_block   air       . . . . .  stable
redstone_block   stone     . . . . .  stable
redstone_block   glass     . . . . .  stable
redstone_block   lamp      . . . . .  stable
redstone_block   dust      J J J J .  stable
stone            air       . . . . .  stable
stone            stone     . . . . .  stable
stone            glass     . . . . .  stable
stone            lamp      . . . . .  stable
stone            dust      . . . . .  stable
```

## Table 3b — the same sweep with the driver standing *on* the mediator

The mirror, and the one geometry in which a dust driver actually powers the mediator: dust weakly powers the block it stands on, and nothing else. So this table is where mechanism 4 — weak power reaching a block and stopping there — is visible, in the `D` column of the `dust` rows.

`torch` here stands on the mediator, so the mediator is its own support: its whole row is the statement that a torch does not power what it stands on.

```
driver           mediator  N S E W D  settle
lever            air       . . . . .  stable
lever            stone     J J J J J  stable
lever            glass     . . . . .  stable
lever            lamp      J J J J J  stable
lever            dust      J J J J .  stable
torch            air       . . . . .  stable
torch            stone     . . . . .  stable
torch            glass     . . . . .  stable
torch            lamp      . . . . .  stable
torch            dust      . . . . .  stable
wall_torch       air       . . . . .  stable
wall_torch       stone     . . . . .  stable
wall_torch       glass     . . . . .  stable
wall_torch       lamp      . . . . .  stable
wall_torch       dust      J J J J .  stable
repeater         air       x . . . .  stable
repeater         stone     x . . . .  stable
repeater         glass     x . . . .  stable
repeater         lamp      x . . . .  stable
repeater         dust      x . . . .  stable
comparator       air       x . . . .  stable
comparator       stone     x . . . .  stable
comparator       glass     x . . . .  stable
comparator       lamp      x . . . .  stable
comparator       dust      x . . . .  stable
dust             air       J J J J .  stable
dust             stone     j j j j .  stable
dust             glass     j j j j .  stable
dust             lamp      j j j j .  stable
dust             dust      J J J J .  stable
redstone_block   air       . . . . .  stable
redstone_block   stone     . . . . .  stable
redstone_block   glass     . . . . .  stable
redstone_block   lamp      . . . . .  stable
redstone_block   dust      J J J J .  stable
stone            air       . . . . .  stable
stone            stone     . . . . .  stable
stone            glass     . . . . .  stable
stone            lamp      . . . . .  stable
stone            dust      . . . . .  stable
```

## Table 4 — a one-way dust edge, and what the walk's seed order does with it

Two wires one cardinal step apart with the second a layer down. `floor` is the cell under the **upper** wire; `mid` is the cell between them. `up→lo` drives the upper wire and reads the lower one, `lo→up` is the mirror rig, and `walk` is whether `verify_connectivity`'s own walk puts the two in one component.

This is still mechanism 1 — dust against dust, the one relation the walk is supposed to cover — so every coupled row here should be `j`. The rows where it is `J` instead are rows where the edge exists in **one direction only** and the walk's seed order runs against it: seeds arrive in `positions_of` order, which is flat-index order, which is lowest `y` first, and a cell already claimed by an earlier seed's component is skipped with `continue` before any owner is compared. So a descend-only edge — upper drives lower, lower cannot climb back — is walked from the lower cell first, finds nothing, and the upper cell then forms a component of its own that never meets it.

**NOT MEASURED: whether any circuit this compiler builds contains such a pair.** The structural condition is that the upper wire's own floor does not support a dust step, which is not the shape `Case::as_built` in `tests/dust_join_relation.rs` reports the router producing. Nothing here says it cannot happen; nothing here says it does.

```
floor   mid     up→lo  lo→up  walk
air     air     J      .      false
air     stone   .      .      false
air     glass   .      .      false
stone   air     j      j      true
stone   stone   .      .      false
stone   glass   .      j      true
glass   air     j      j      true
glass   stone   .      .      false
glass   glass   .      j      true
```

## Table 5 — the receiver decides whether weak power couples

Tables 1 to 3 all read a dust cell, and dust is only ever re-driven by **strong** block power (`recompute_dust_strengths` seeds a wire from `block_signal_at` only when the answer is `BlockPower::Strong`). Four other things this compiler writes can read a block, and two of them accept weak power. That is a whole second class of edge, and no dust probe anywhere can see it.

`across` puts the driver on top of the mediator so the coupling has to cross it — a lever there powers it strongly, a dust cell standing on it powers it weakly. `beside` moves the driver next to the receiver instead and leaves the mediator unpowered, which is the control for the same row: a mark under `beside` is a direct component-to-receiver edge, and its absence says the receiver reads only the one cell it is supposed to.

The mediator is stone throughout. `dust` and `lamp` hang under it; `wall_torch`, `repeater` and `comparator` stand north of it, each oriented so the mediator is the cell it reads.

```
path    driver                           dust       lamp       wall_torch repeater   comparator  settle
across  lever (strong)                   J          .          J          J          J           stable
across  dust (weak)                      .          .          J          J          J           stable
across  redstone_block (no block power)  .          .          .          .          .           stable
across  stone (inert)                    .          .          .          .          .           stable
beside  lever (strong)                   J          J          .          .          .           stable
beside  dust (weak)                      j          J          .          .          .           stable
beside  redstone_block (no block power)  J          .          .          .          .           stable
beside  stone (inert)                    .          .          .          .          .           stable
```

## Table 6 — the taxonomy's own answer, for comparison

`taxonomy::power_emitted_toward` asked in all six directions, with no world at all. `S` strong block power · `w` weak block power · `d` drives dust but powers no block · `-` inert. Read against Table 2: an `S` here is what a `J` there is made of, and a `w` is what a `.` there is made of.

The dust row reads `-` in five of six directions and that is **not** a claim that dust is inert horizontally — `power_emitted_toward` has no world and therefore cannot know a wire's connection shape. The world-aware answer is `propagate::dust_power_toward`, and Table 3b measures it.

The three rows to look at are `lever`, `button` and `pressure_plate`. Their `face` is not modelled by `power_emitted_toward` — they fall through its `_ => full` arm — so the taxonomy gives them **strong power in all six directions**. Vanilla gives a `face=floor` lever strong power to the block **below** it only.

```
emitter          facing  N S E W U D
stone            -       - - - - - -
glass            -       - - - - - -
lamp             -       - - - - - -
redstone_block   -       d d d d d d
dust             -       - - - - - -
repeater         N       - - - - - -
repeater         S       - - - - - -
repeater         E       - - - - - -
repeater         W       - - - - - -
comparator       N       - - - - - -
comparator       S       - - - - - -
comparator       E       - - - - - -
comparator       W       - - - - - -
torch            -       w w w w S -
wall_torch       N       w - w w S w
wall_torch       S       - w w w S w
wall_torch       E       w w w - S w
wall_torch       W       w w - w S w
lever            -       S S S S S S
button           -       S S S S S S
pressure_plate   -       S S S S S S
```

