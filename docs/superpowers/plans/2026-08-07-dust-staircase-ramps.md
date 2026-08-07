# Replace repeater ramps with dust staircases

## What the measurement said

`docs/superpowers/specs/2026-08-07-routing-cost-breakdown.md` decomposed every
routed edge in all four reference circuits. The result:

| circuit | ramp | gate-entry | track | column |
|---|---|---|---|---|
| and4 | **66%** | 28% | 3% | 3% |
| seven_segment | **45%** | 35% | 17% | 3% |

Ramp is **exactly 4 repeaters on every critical-path edge in every circuit** —
two levels up, two levels down, built out of repeaters — regardless of how far
apart the two gates are. That fixed tax is why the delay ratio is already 7.25x
on a 7-gate circuit.

## The change

Redstone dust climbs a staircase without repeaters. The cost is **signal
strength**, not time: one level of climb costs the same one strength per block
that flat dust costs. Four repeaters become zero, and four redstone ticks per
hop become zero.

This was considered and deliberately not taken when the compact layout landed.
The note then read: a dust-staircase ramp would allow tighter track spacing "at
the cost of hand-managing signal strength — I chose the already-proven repeater
ramp since the criterion was met." That decision was made against a **size**
target that had already been satisfied. The target now is delay, and the same
trade goes the other way.

## What it costs, and what has to be handled

**Strength budget.** `MAX_DUST_RUN = 14` assumes flat dust with a fresh 15 at
the start of each run. A staircase spends strength on the climb, so the flat run
either side of it must be shortened by whatever the ramp consumed. Getting this
wrong does not produce a routing error — it produces a wire that silently dies
partway along, and the circuit is simply wrong. The strength accounting has to
be derived, not guessed, and it has to be checked by the simulator.

**Cross-talk.** The repeater ramp was chosen partly because of what a descending
ramp leaves behind: its last step strongly powers a block one layer under the
track plane, which at tighter track spacing would drive an unrelated track. A
dust staircase has a different footprint and a different power signature. Do not
assume it is safe because it is smaller — establish what it powers and confirm
nothing else is in reach.

**The isolation floor.** `Y=2` exists to physically block redstone's climb and
diagonal-descend rules from bridging the gate plane and the track plane. A ramp
is by definition a hole through that floor. The hole must be exactly where the
ramp is and nowhere else.

## The prediction

The validated delay model from the breakdown is

```
settle ticks = logic_depth_bound + 2 x (critical-path repeaters) + 2
```

It reproduces all four measured settle times exactly (58, 144, 148, 166).

`seven_segment`'s critical path is 9 hops carrying 73 repeaters. Removing 4 per
hop leaves 37, predicting **166 -> 94 game ticks**, a 1.77x improvement.

Report the measured result against this prediction. A large miss means either
the model or the change is not what we think it is, and that is worth more than
the speedup.

## Out of scope

- Gate-entry cost (the mandatory socket-terminating repeater and the corner
  turn), which is the next 27-35%. Separate task.
- Track spacing. A dust staircase may permit tighter spacing than 5, but that is
  a size win, not a delay win, and mixing it in here would confound the
  measurement this task exists to produce.
- Anything 3D.
