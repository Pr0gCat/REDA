# The realised-graph extractor, differenced against the simulator

**Generated. Do not edit by hand.** Regenerate with

```
cargo test --release --lib -- --ignored compile::coupling::tests::regenerate_the_calibration_table
```

and `the_extractor_agrees_with_the_simulator` in `src/compile/coupling/tests.rs` fails if this text and the extractor ever disagree.

Each row is one emitter, at one facing, with one mediator material between it and a bare dust receiver. Each column is the direction the receiver lies in. The emitter is driven, the world is run to stable, the receiver's dust strength is read, and the whole thing is repeated with the emitter cell written as **air** as a control; a coupling is a reading that changed. `compile::coupling::reach_of` -- the same walk the circuit sweep uses -- is then asked whether it reaches the same cell, and the two answers are compared.

`J` both say coupled · `.` both say clear · `+` **extractor claims an edge the simulator does not have** · `X` **simulator has an edge the extractor cannot see** · `~` contaminated (the control was not quiescent) · `x` rig invalid (the feed would touch a cell under test) · `!` diverged.

Button and pressure plate are absent on purpose: `run_until_stable` refuses any world containing one, so no differenced reading exists for them. `docs/derived/coupling-mechanisms.md` reports their load-only rows.

```
emitter          facing  mediator  N S E W U D
stone            -       -         . . . . . .
stone            -       air       . . . . . .
stone            -       stone     . . . . . .
stone            -       glass     . . . . . .
stone            -       lamp      . . . . . .
stone            -       dust      . . . . . .
glass            -       -         . . . . . .
glass            -       air       . . . . . .
glass            -       stone     . . . . . .
glass            -       glass     . . . . . .
glass            -       lamp      . . . . . .
glass            -       dust      . . . . . .
lamp             -       -         . . . . . .
lamp             -       air       . . . . . .
lamp             -       stone     . . . . . .
lamp             -       glass     . . . . . .
lamp             -       lamp      . . . . . .
lamp             -       dust      . . . . . .
redstone_block   -       -         J J J J J J
redstone_block   -       air       . . . . . .
redstone_block   -       stone     . . . . . .
redstone_block   -       glass     . . . . . .
redstone_block   -       lamp      . . . . . .
redstone_block   -       dust      J J J J . .
dust             -       -         x J J J . .
dust             -       air       x . . . . .
dust             -       stone     x . . . . .
dust             -       glass     x . . . . .
dust             -       lamp      x . . . . .
dust             -       dust      x J J J . .
repeater         N       -         x J . . . .
repeater         N       air       x . . . . .
repeater         N       stone     x J . . . .
repeater         N       glass     x . . . . .
repeater         N       lamp      x J . . . .
repeater         N       dust      x J . . . .
repeater         S       -         J x . . . .
repeater         S       air       . x . . . .
repeater         S       stone     J x . . . .
repeater         S       glass     . x . . . .
repeater         S       lamp      J x . . . .
repeater         S       dust      J x . . . .
repeater         E       -         . . x J . .
repeater         E       air       . . x . . .
repeater         E       stone     . . x J . .
repeater         E       glass     . . x . . .
repeater         E       lamp      . . x J . .
repeater         E       dust      . . x J . .
repeater         W       -         . . J x . .
repeater         W       air       . . . x . .
repeater         W       stone     . . J x . .
repeater         W       glass     . . . x . .
repeater         W       lamp      . . J x . .
repeater         W       dust      . . J x . .
comparator       N       -         x J . . . .
comparator       N       air       x . . . . .
comparator       N       stone     x J . . . .
comparator       N       glass     x . . . . .
comparator       N       lamp      x J . . . .
comparator       N       dust      x J . . . .
comparator       S       -         J x . . . .
comparator       S       air       . x . . . .
comparator       S       stone     J x . . . .
comparator       S       glass     . x . . . .
comparator       S       lamp      J x . . . .
comparator       S       dust      J x . . . .
comparator       E       -         . . x J . .
comparator       E       air       . . x . . .
comparator       E       stone     . . x J . .
comparator       E       glass     . . x . . .
comparator       E       lamp      . . x J . .
comparator       E       dust      . . x J . .
comparator       W       -         . . J x . .
comparator       W       air       . . . x . .
comparator       W       stone     . . J x . .
comparator       W       glass     . . . x . .
comparator       W       lamp      . . J x . .
comparator       W       dust      . . J x . .
torch            -       -         J J J J J .
torch            -       air       . . . . . .
torch            -       stone     . . . . J .
torch            -       glass     . . . . . .
torch            -       lamp      . . . . J .
torch            -       dust      J J J J . .
wall_torch       N       -         J . J J J J
wall_torch       N       air       . . . . . .
wall_torch       N       stone     . . . . J .
wall_torch       N       glass     . . . . . .
wall_torch       N       lamp      . . . . J .
wall_torch       N       dust      J . J J . .
wall_torch       S       -         . J J J J J
wall_torch       S       air       . . . . . .
wall_torch       S       stone     . . . . J .
wall_torch       S       glass     . . . . . .
wall_torch       S       lamp      . . . . J .
wall_torch       S       dust      . J J J . .
wall_torch       E       -         J J J . J J
wall_torch       E       air       . . . . . .
wall_torch       E       stone     . . . . J .
wall_torch       E       glass     . . . . . .
wall_torch       E       lamp      . . . . J .
wall_torch       E       dust      J J J . . .
wall_torch       W       -         J J . J J J
wall_torch       W       air       . . . . . .
wall_torch       W       stone     . . . . J .
wall_torch       W       glass     . . . . . .
wall_torch       W       lamp      . . . . J .
wall_torch       W       dust      J J . J . .
lever            -       -         J J J J J J
lever            -       air       . . . . . .
lever            -       stone     J J J J J J
lever            -       glass     . . . . . .
lever            -       lamp      J J J J J J
lever            -       dust      J J J J . .
```

## Summary, computed from the runs above

* 684 rigs measured, of which **121 couple** and 509 do not.
* **0 disagreements** between the extractor and the simulator.
* 54 rigs were refused as invalid (the feed would have touched a cell under test) and 0 were contaminated.
