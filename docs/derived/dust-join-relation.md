# The dust-join relation, measured

**Generated. Do not edit by hand.** Every row below is a `Simulator` run, not a reading of the rules. Regenerate with

```
cargo test --release --test dust_join_relation -- --ignored regenerate_the_dust_join_table
```

and `the_committed_table_is_what_the_simulator_says_today` in the same file fails if this text and the simulator ever disagree.

Method, in full, lives in that file's module doc comment. In short: two dust cells at a relative offset; one is driven by a redstone block through a 3-cell feed run laid away from the other; the world is run to stable; the other cell's power is read; and the whole thing is repeated with the driven cell deleted, as a control. A reading that survives the control is `contaminated` and is never counted as a join.

## Summary, computed from the runs below

* **12 of `keep_out`'s 12 cells really join** in the shape a compiled world presents: every dust cell on a stone floor, nothing laid above it. So in the world the router actually builds, `keep_out` is not conservative at all — it is exact.
* **8 of the 12 are nonetheless conditional**: there is some content of one single cell that stops the pair being one network. They are [(0, -1, -1), (0, 1, -1), (0, -1, 1), (0, 1, 1), (1, -1, 0), (1, 1, 0), (-1, -1, 0), (-1, 1, 0)] — every offset with a level change, and none without.
* The one cell is always **the cell directly above the lower conductor**. Two independent properties of it are read, with opposite polarity: the higher cell descends when that cell does *not* support a dust step, and the lower cell climbs when it does *not* conduct.
* The four same-layer cells are **unconditional**. No content of any cell anywhere makes two dust cells one cardinal step apart at the same Y stop being one network; the simulator's own `dust_connections` has no branch that could.
* `CONDUCTOR_CLEARANCE = 2` holds exactly: touching reads `join`, one clear cell between reads `no`.

The closed form, as `directed_join` in the generator states it and `the_closed_form_rule_reproduces_every_attributable_row` checks it against every run here. With `S` the cell below the higher conductor and `C` the cell above the lower one, for a pair one cardinal step apart horizontally:

```
same layer          joined, unconditionally, both ways
lower -> higher     supports_dust_step(S) && !is_conductive(C)
higher -> lower     !supports_dust_step(C)
anything else       never
```

Either direction is enough to merge two nets: `verify_connectivity` walks `dust_connections` forward from every dust cell, so a one-way edge is still a short.

## Table 1 — every offset, in the shape a compiled world presents

Both conductors are bare dust on a stone floor; nothing is laid above either. `A->B` drives A and reads B; `B->A` is the mirror rig. `reach` is `connectivity::dust_reach` asked whether it names the read cell directly, in the very same world. `keep_out` is whether `planner.rs` refuses the offset.

```
offset(dx,dy,dz)  A->B    B->A    reach(A->B)  reach(B->A)  keep_out
(-2,-1,-2)        no      no      false        false        false
(-1,-1,-2)        no      no      false        false        false
(0,-1,-2)         no      no      false        false        false
(1,-1,-2)         no      no      false        false        false
(2,-1,-2)         no      no      false        false        false
(-2,-1,-1)        no      no      false        false        false
(-1,-1,-1)        no      no      false        false        false
(0,-1,-1)         join    join    true         true         true
(1,-1,-1)         no      no      false        false        false
(2,-1,-1)         no      no      false        false        false
(-2,-1,0)         no      no      false        false        false
(-1,-1,0)         join    join    true         true         true
(0,-1,0)          contaminated no      false        false        false
(1,-1,0)          join    join    true         true         true
(2,-1,0)          no      no      false        false        false
(-2,-1,1)         no      no      false        false        false
(-1,-1,1)         no      no      false        false        false
(0,-1,1)          join    join    true         true         true
(1,-1,1)          no      no      false        false        false
(2,-1,1)          no      no      false        false        false
(-2,-1,2)         no      no      false        false        false
(-1,-1,2)         no      no      false        false        false
(0,-1,2)          no      no      false        false        false
(1,-1,2)          no      no      false        false        false
(2,-1,2)          no      no      false        false        false
(-2,0,-2)         no      no      false        false        false
(-1,0,-2)         no      no      false        false        false
(0,0,-2)          no      no      false        false        false
(1,0,-2)          no      no      false        false        false
(2,0,-2)          no      no      false        false        false
(-2,0,-1)         no      no      false        false        false
(-1,0,-1)         no      no      false        false        false
(0,0,-1)          join    join    true         true         true
(1,0,-1)          no      no      false        false        false
(2,0,-1)          no      no      false        false        false
(-2,0,0)          no      no      false        false        false
(-1,0,0)          join    join    true         true         true
(1,0,0)           join    join    true         true         true
(2,0,0)           no      no      false        false        false
(-2,0,1)          no      no      false        false        false
(-1,0,1)          no      no      false        false        false
(0,0,1)           join    join    true         true         true
(1,0,1)           no      no      false        false        false
(2,0,1)           no      no      false        false        false
(-2,0,2)          no      no      false        false        false
(-1,0,2)          no      no      false        false        false
(0,0,2)           no      no      false        false        false
(1,0,2)           no      no      false        false        false
(2,0,2)           no      no      false        false        false
(-2,1,-2)         no      no      false        false        false
(-1,1,-2)         no      no      false        false        false
(0,1,-2)          no      no      false        false        false
(1,1,-2)          no      no      false        false        false
(2,1,-2)          no      no      false        false        false
(-2,1,-1)         no      no      false        false        false
(-1,1,-1)         no      no      false        false        false
(0,1,-1)          join    join    true         true         true
(1,1,-1)          no      no      false        false        false
(2,1,-1)          no      no      false        false        false
(-2,1,0)          no      no      false        false        false
(-1,1,0)          join    join    true         true         true
(0,1,0)           no      contaminated false        false        false
(1,1,0)           join    join    true         true         true
(2,1,0)           no      no      false        false        false
(-2,1,1)          no      no      false        false        false
(-1,1,1)          no      no      false        false        false
(0,1,1)           join    join    true         true         true
(1,1,1)           no      no      false        false        false
(2,1,1)           no      no      false        false        false
(-2,1,2)          no      no      false        false        false
(-1,1,2)          no      no      false        false        false
(0,1,2)           no      no      false        false        false
(1,1,2)           no      no      false        false        false
(2,1,2)           no      no      false        false        false
```

## Table 2 — the two cells the vertical arms read, varied

East only, so the two varied cells are enumerated fully; Table 2b repeats the gate sweep in all four directions. `gate` is the cell one step east of A at A's own level — B's floor for the climb, the cell above B for the descend. `ceiling` is `A.up()`.

```
offset(dx,dy,dz)  gate            ceiling         A->B    B->A    reach(A->B)  reach(B->A)
(1,1,0)           air             air             no      join    false        true
(1,1,0)           air             solid           no      no      false        false
(1,1,0)           air             dust            no      join    false        true
(1,1,0)           air             repeater        no      join    false        true
(1,1,0)           air             torch           contaminated join    false        true
(1,1,0)           air             wall_torch      contaminated contaminated false        true
(1,1,0)           air             lever           no      join    false        true
(1,1,0)           air             lamp            no      no      false        false
(1,1,0)           air             glass           no      no      false        false
(1,1,0)           air             redstone_block  contaminated contaminated false        false
(1,1,0)           solid           air             join    join    true         true
(1,1,0)           solid           solid           no      no      false        false
(1,1,0)           solid           dust            join    join    true         true
(1,1,0)           solid           repeater        join    join    true         true
(1,1,0)           solid           torch           contaminated join    true         true
(1,1,0)           solid           wall_torch      contaminated contaminated true         true
(1,1,0)           solid           lever           join    join    true         true
(1,1,0)           solid           lamp            no      no      false        false
(1,1,0)           solid           glass           join    no      true         false
(1,1,0)           solid           redstone_block  contaminated contaminated true         false
(1,1,0)           dust            air             no      contaminated false        true
(1,1,0)           dust            solid           no      contaminated false        false
(1,1,0)           dust            dust            no      contaminated false        true
(1,1,0)           dust            repeater        no      contaminated false        true
(1,1,0)           dust            torch           contaminated contaminated false        true
(1,1,0)           dust            wall_torch      contaminated contaminated false        true
(1,1,0)           dust            lever           no      contaminated false        true
(1,1,0)           dust            lamp            no      contaminated false        false
(1,1,0)           dust            glass           no      contaminated false        false
(1,1,0)           dust            redstone_block  contaminated contaminated false        false
(1,1,0)           repeater        air             no      join    false        true
(1,1,0)           repeater        solid           no      no      false        false
(1,1,0)           repeater        dust            no      join    false        true
(1,1,0)           repeater        repeater        no      join    false        true
(1,1,0)           repeater        torch           contaminated join    false        true
(1,1,0)           repeater        wall_torch      contaminated contaminated false        true
(1,1,0)           repeater        lever           no      join    false        true
(1,1,0)           repeater        lamp            no      no      false        false
(1,1,0)           repeater        glass           no      no      false        false
(1,1,0)           repeater        redstone_block  contaminated contaminated false        false
(1,1,0)           torch           air             contaminated contaminated false        true
(1,1,0)           torch           solid           contaminated contaminated false        false
(1,1,0)           torch           dust            contaminated contaminated false        true
(1,1,0)           torch           repeater        contaminated contaminated false        true
(1,1,0)           torch           torch           contaminated contaminated false        true
(1,1,0)           torch           wall_torch      contaminated contaminated false        true
(1,1,0)           torch           lever           contaminated contaminated false        true
(1,1,0)           torch           lamp            contaminated contaminated false        false
(1,1,0)           torch           glass           contaminated contaminated false        false
(1,1,0)           torch           redstone_block  contaminated contaminated false        false
(1,1,0)           wall_torch      air             contaminated contaminated false        true
(1,1,0)           wall_torch      solid           contaminated contaminated false        false
(1,1,0)           wall_torch      dust            contaminated contaminated false        true
(1,1,0)           wall_torch      repeater        contaminated contaminated false        true
(1,1,0)           wall_torch      torch           contaminated contaminated false        true
(1,1,0)           wall_torch      wall_torch      contaminated contaminated false        true
(1,1,0)           wall_torch      lever           contaminated contaminated false        true
(1,1,0)           wall_torch      lamp            contaminated contaminated false        false
(1,1,0)           wall_torch      glass           contaminated contaminated false        false
(1,1,0)           wall_torch      redstone_block  contaminated contaminated false        false
(1,1,0)           lever           air             no      join    false        true
(1,1,0)           lever           solid           no      no      false        false
(1,1,0)           lever           dust            no      join    false        true
(1,1,0)           lever           repeater        no      join    false        true
(1,1,0)           lever           torch           contaminated join    false        true
(1,1,0)           lever           wall_torch      contaminated contaminated false        true
(1,1,0)           lever           lever           no      join    false        true
(1,1,0)           lever           lamp            no      no      false        false
(1,1,0)           lever           glass           no      no      false        false
(1,1,0)           lever           redstone_block  contaminated contaminated false        false
(1,1,0)           lamp            air             join    join    true         true
(1,1,0)           lamp            solid           no      no      false        false
(1,1,0)           lamp            dust            join    join    true         true
(1,1,0)           lamp            repeater        join    join    true         true
(1,1,0)           lamp            torch           contaminated join    true         true
(1,1,0)           lamp            wall_torch      contaminated contaminated true         true
(1,1,0)           lamp            lever           join    join    true         true
(1,1,0)           lamp            lamp            no      no      false        false
(1,1,0)           lamp            glass           join    no      true         false
(1,1,0)           lamp            redstone_block  contaminated contaminated true         false
(1,1,0)           glass           air             join    join    true         true
(1,1,0)           glass           solid           no      no      false        false
(1,1,0)           glass           dust            join    join    true         true
(1,1,0)           glass           repeater        join    join    true         true
(1,1,0)           glass           torch           contaminated join    true         true
(1,1,0)           glass           wall_torch      contaminated contaminated true         true
(1,1,0)           glass           lever           join    join    true         true
(1,1,0)           glass           lamp            no      no      false        false
(1,1,0)           glass           glass           join    no      true         false
(1,1,0)           glass           redstone_block  contaminated contaminated true         false
(1,1,0)           redstone_block  air             contaminated contaminated true         true
(1,1,0)           redstone_block  solid           contaminated contaminated false        false
(1,1,0)           redstone_block  dust            contaminated contaminated true         true
(1,1,0)           redstone_block  repeater        contaminated contaminated true         true
(1,1,0)           redstone_block  torch           contaminated contaminated true         true
(1,1,0)           redstone_block  wall_torch      contaminated contaminated true         true
(1,1,0)           redstone_block  lever           contaminated contaminated true         true
(1,1,0)           redstone_block  lamp            contaminated contaminated false        false
(1,1,0)           redstone_block  glass           contaminated contaminated true         false
(1,1,0)           redstone_block  redstone_block  contaminated contaminated true         false
(1,-1,0)          air             air             join    join    true         true
(1,-1,0)          air             solid           join    join    true         true
(1,-1,0)          air             dust            join    join    true         true
(1,-1,0)          air             repeater        join    join    true         true
(1,-1,0)          air             torch           join    join    true         true
(1,-1,0)          air             wall_torch      join    contaminated true         true
(1,-1,0)          air             lever           join    join    true         true
(1,-1,0)          air             lamp            join    join    true         true
(1,-1,0)          air             glass           join    join    true         true
(1,-1,0)          air             redstone_block  join    contaminated true         true
(1,-1,0)          solid           air             no      no      false        false
(1,-1,0)          solid           solid           no      no      false        false
(1,-1,0)          solid           dust            no      no      false        false
(1,-1,0)          solid           repeater        no      no      false        false
(1,-1,0)          solid           torch           no      no      false        false
(1,-1,0)          solid           wall_torch      no      contaminated false        false
(1,-1,0)          solid           lever           no      no      false        false
(1,-1,0)          solid           lamp            no      no      false        false
(1,-1,0)          solid           glass           no      no      false        false
(1,-1,0)          solid           redstone_block  no      contaminated false        false
(1,-1,0)          dust            air             join    join    true         true
(1,-1,0)          dust            solid           join    join    true         true
(1,-1,0)          dust            dust            join    join    true         true
(1,-1,0)          dust            repeater        join    join    true         true
(1,-1,0)          dust            torch           join    join    true         true
(1,-1,0)          dust            wall_torch      join    contaminated true         true
(1,-1,0)          dust            lever           join    join    true         true
(1,-1,0)          dust            lamp            join    join    true         true
(1,-1,0)          dust            glass           join    join    true         true
(1,-1,0)          dust            redstone_block  join    contaminated true         true
(1,-1,0)          repeater        air             join    join    true         true
(1,-1,0)          repeater        solid           join    join    true         true
(1,-1,0)          repeater        dust            join    join    true         true
(1,-1,0)          repeater        repeater        join    join    true         true
(1,-1,0)          repeater        torch           join    join    true         true
(1,-1,0)          repeater        wall_torch      join    contaminated true         true
(1,-1,0)          repeater        lever           join    join    true         true
(1,-1,0)          repeater        lamp            join    join    true         true
(1,-1,0)          repeater        glass           join    join    true         true
(1,-1,0)          repeater        redstone_block  join    contaminated true         true
(1,-1,0)          torch           air             join    contaminated true         true
(1,-1,0)          torch           solid           join    contaminated true         true
(1,-1,0)          torch           dust            join    contaminated true         true
(1,-1,0)          torch           repeater        join    contaminated true         true
(1,-1,0)          torch           torch           join    contaminated true         true
(1,-1,0)          torch           wall_torch      join    contaminated true         true
(1,-1,0)          torch           lever           join    contaminated true         true
(1,-1,0)          torch           lamp            join    contaminated true         true
(1,-1,0)          torch           glass           join    contaminated true         true
(1,-1,0)          torch           redstone_block  join    contaminated true         true
(1,-1,0)          wall_torch      air             contaminated contaminated true         true
(1,-1,0)          wall_torch      solid           contaminated contaminated true         true
(1,-1,0)          wall_torch      dust            contaminated contaminated true         true
(1,-1,0)          wall_torch      repeater        contaminated contaminated true         true
(1,-1,0)          wall_torch      torch           contaminated contaminated true         true
(1,-1,0)          wall_torch      wall_torch      contaminated contaminated true         true
(1,-1,0)          wall_torch      lever           contaminated contaminated true         true
(1,-1,0)          wall_torch      lamp            contaminated contaminated true         true
(1,-1,0)          wall_torch      glass           contaminated contaminated true         true
(1,-1,0)          wall_torch      redstone_block  contaminated contaminated true         true
(1,-1,0)          lever           air             join    join    true         true
(1,-1,0)          lever           solid           join    join    true         true
(1,-1,0)          lever           dust            join    join    true         true
(1,-1,0)          lever           repeater        join    join    true         true
(1,-1,0)          lever           torch           join    join    true         true
(1,-1,0)          lever           wall_torch      join    contaminated true         true
(1,-1,0)          lever           lever           join    join    true         true
(1,-1,0)          lever           lamp            join    join    true         true
(1,-1,0)          lever           glass           join    join    true         true
(1,-1,0)          lever           redstone_block  join    contaminated true         true
(1,-1,0)          lamp            air             no      no      false        false
(1,-1,0)          lamp            solid           no      no      false        false
(1,-1,0)          lamp            dust            no      no      false        false
(1,-1,0)          lamp            repeater        no      no      false        false
(1,-1,0)          lamp            torch           no      no      false        false
(1,-1,0)          lamp            wall_torch      no      contaminated false        false
(1,-1,0)          lamp            lever           no      no      false        false
(1,-1,0)          lamp            lamp            no      no      false        false
(1,-1,0)          lamp            glass           no      no      false        false
(1,-1,0)          lamp            redstone_block  no      contaminated false        false
(1,-1,0)          glass           air             no      join    false        true
(1,-1,0)          glass           solid           no      join    false        true
(1,-1,0)          glass           dust            no      join    false        true
(1,-1,0)          glass           repeater        no      join    false        true
(1,-1,0)          glass           torch           no      join    false        true
(1,-1,0)          glass           wall_torch      no      contaminated false        true
(1,-1,0)          glass           lever           no      join    false        true
(1,-1,0)          glass           lamp            no      join    false        true
(1,-1,0)          glass           glass           no      join    false        true
(1,-1,0)          glass           redstone_block  no      contaminated false        true
(1,-1,0)          redstone_block  air             contaminated contaminated false        true
(1,-1,0)          redstone_block  solid           contaminated contaminated false        true
(1,-1,0)          redstone_block  dust            contaminated contaminated false        true
(1,-1,0)          redstone_block  repeater        contaminated contaminated false        true
(1,-1,0)          redstone_block  torch           contaminated contaminated false        true
(1,-1,0)          redstone_block  wall_torch      contaminated contaminated false        true
(1,-1,0)          redstone_block  lever           contaminated contaminated false        true
(1,-1,0)          redstone_block  lamp            contaminated contaminated false        true
(1,-1,0)          redstone_block  glass           contaminated contaminated false        true
(1,-1,0)          redstone_block  redstone_block  contaminated contaminated false        true
```

## Table 2b — the same gate sweep, all four directions

`ceiling` is air throughout. This is the symmetry check: the relation must not depend on which cardinal axis it is measured along.

```
offset(dx,dy,dz)  gate            A->B    B->A    reach(A->B)  reach(B->A)
(1,1,0)           air             no      join    false        true
(1,1,0)           solid           join    join    true         true
(1,1,0)           dust            no      contaminated false        true
(1,1,0)           repeater        no      join    false        true
(1,1,0)           torch           contaminated contaminated false        true
(1,1,0)           wall_torch      contaminated contaminated false        true
(1,1,0)           lever           no      join    false        true
(1,1,0)           lamp            join    join    true         true
(1,1,0)           glass           join    join    true         true
(1,1,0)           redstone_block  contaminated contaminated true         true
(-1,1,0)          air             no      join    false        true
(-1,1,0)          solid           join    join    true         true
(-1,1,0)          dust            no      contaminated false        true
(-1,1,0)          repeater        no      join    false        true
(-1,1,0)          torch           contaminated contaminated false        true
(-1,1,0)          wall_torch      contaminated contaminated false        true
(-1,1,0)          lever           no      join    false        true
(-1,1,0)          lamp            join    join    true         true
(-1,1,0)          glass           join    join    true         true
(-1,1,0)          redstone_block  contaminated contaminated true         true
(0,1,1)           air             no      join    false        true
(0,1,1)           solid           join    join    true         true
(0,1,1)           dust            no      contaminated false        true
(0,1,1)           repeater        no      join    false        true
(0,1,1)           torch           contaminated contaminated false        true
(0,1,1)           wall_torch      contaminated join    false        true
(0,1,1)           lever           no      join    false        true
(0,1,1)           lamp            join    join    true         true
(0,1,1)           glass           join    join    true         true
(0,1,1)           redstone_block  contaminated contaminated true         true
(0,1,-1)          air             no      join    false        true
(0,1,-1)          solid           join    join    true         true
(0,1,-1)          dust            no      contaminated false        true
(0,1,-1)          repeater        no      contaminated false        true
(0,1,-1)          torch           contaminated contaminated false        true
(0,1,-1)          wall_torch      contaminated join    false        true
(0,1,-1)          lever           no      join    false        true
(0,1,-1)          lamp            join    join    true         true
(0,1,-1)          glass           join    join    true         true
(0,1,-1)          redstone_block  contaminated contaminated true         true
(1,-1,0)          air             join    join    true         true
(1,-1,0)          solid           no      no      false        false
(1,-1,0)          dust            join    join    true         true
(1,-1,0)          repeater        join    join    true         true
(1,-1,0)          torch           join    contaminated true         true
(1,-1,0)          wall_torch      contaminated contaminated true         true
(1,-1,0)          lever           join    join    true         true
(1,-1,0)          lamp            no      no      false        false
(1,-1,0)          glass           no      join    false        true
(1,-1,0)          redstone_block  contaminated contaminated false        true
(-1,-1,0)         air             join    join    true         true
(-1,-1,0)         solid           no      no      false        false
(-1,-1,0)         dust            join    join    true         true
(-1,-1,0)         repeater        join    join    true         true
(-1,-1,0)         torch           join    contaminated true         true
(-1,-1,0)         wall_torch      contaminated contaminated true         true
(-1,-1,0)         lever           join    join    true         true
(-1,-1,0)         lamp            no      no      false        false
(-1,-1,0)         glass           no      join    false        true
(-1,-1,0)         redstone_block  contaminated contaminated false        true
(0,-1,1)          air             join    join    true         true
(0,-1,1)          solid           no      no      false        false
(0,-1,1)          dust            join    join    true         true
(0,-1,1)          repeater        join    join    true         true
(0,-1,1)          torch           join    contaminated true         true
(0,-1,1)          wall_torch      contaminated contaminated true         true
(0,-1,1)          lever           join    join    true         true
(0,-1,1)          lamp            no      no      false        false
(0,-1,1)          glass           no      join    false        true
(0,-1,1)          redstone_block  contaminated contaminated false        true
(0,-1,-1)         air             join    join    true         true
(0,-1,-1)         solid           no      no      false        false
(0,-1,-1)         dust            join    join    true         true
(0,-1,-1)         repeater        join    join    true         true
(0,-1,-1)         torch           join    contaminated true         true
(0,-1,-1)         wall_torch      contaminated join    true         true
(0,-1,-1)         lever           join    join    true         true
(0,-1,-1)         lamp            no      no      false        false
(0,-1,-1)         glass           no      join    false        true
(0,-1,-1)         redstone_block  contaminated contaminated false        true
```

## Table 3 — a repeater standing where the second conductor would

A is dust, fed as usual. At the offset stands a repeater, and the reading is taken from a dust probe on the repeater's *output* face — so a `join` here means A's signal crossed the repeater. `along` puts A at the repeater's rear (the positive control: this must pass at the same layer, or the rig cannot see anything); `across` turns the repeater ninety degrees, the side-contact geometry `2026-08-09-channel-safety-condition.md` calls a firewall.

```
offset(dx,dy,dz)  orientation  probe   keep_out
(0,-1,-1)         along        join    true
(0,-1,-1)         across       no      true
(0,0,-1)          along        join    true
(0,0,-1)          across       no      true
(0,1,-1)          along        no      true
(0,1,-1)          across       no      true
(0,-1,1)          along        join    true
(0,-1,1)          across       no      true
(0,0,1)           along        join    true
(0,0,1)           across       no      true
(0,1,1)           along        no      true
(0,1,1)           across       no      true
(1,-1,0)          along        join    true
(1,-1,0)          across       no      true
(1,0,0)           along        join    true
(1,0,0)           across       no      true
(1,1,0)           along        no      true
(1,1,0)           across       no      true
(-1,-1,0)         along        join    true
(-1,-1,0)         across       no      true
(-1,0,0)          along        join    true
(-1,0,0)          across       no      true
(-1,1,0)          along        no      true
(-1,1,0)          across       no      true
```
