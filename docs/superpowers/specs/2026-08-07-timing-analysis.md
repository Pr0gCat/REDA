# Timing analysis

## The gap this closes

Every test this project has passes by running the circuit to a stable state and
comparing against a truth table. That is a real check and it has caught real
bugs, but it is blind to everything that happens on the way there.

When two paths into the same gate have different lengths, the output takes the
wrong value first and corrects itself when the slower input lands. That is a
glitch, and a settled-state test cannot see one. Today it does not matter —
outputs drive lamps, and a lamp that flickers is still readable. It matters
completely once a signal reaches a latch, because a glitch clocked into a
register is not a flicker, it is wrong data.

There is a second gap. `seven_segment` settles in 158 game ticks and we do not
know how that number is spent: which path is longest, which is shortest, how far
apart they are, or how much of it is logic rather than wire. Without that, every
layout change is judged by one aggregate number that says whether things got
better but never why.

## What gets measured

**Per-net arrival time.** For a given input transition, the game tick at which
each net last changes value. The simulator already advances one tick at a time;
this only requires watching the positions the compiler already knows.

**Glitch count.** How many times each net changes during one settle. More than
once means the net took a wrong value before its final one. This falls out of
the same observation for free, and it is the thing no existing test can see.

**Critical path.** The latest-arriving output, and the chain of nets leading to
it — found by walking the netlist backwards from that output, taking the
latest-arriving input at each step.

**The logic-depth lower bound.** The netlist's longest gate chain, times one
redstone tick per gate. This is what the circuit would cost if wire were free.
The ratio of measured delay to this bound is the single most useful number this
project can report about itself: it is exactly the fraction of latency that is
routing rather than computation, and it is what every layout change should be
judged against.

## Worst case, not a sample

Arrival times depend on which input changed and what the other inputs were.
A single measurement is an anecdote. Everything above is reported as the worst
case across all input vectors the circuit's truth-table test already sweeps —
the same sweep, instrumented.

## Shape of the implementation

The simulator gains an optional observer: a set of watched positions, and a log
of `(tick, position, new value)` for those positions only. Watching a set rather
than the whole world keeps the cost proportional to the number of nets, not the
world volume — the same principle that made propagation sparse.

`compile()` already returns each gate's output position, so wiring every net into
the observer needs no new placement knowledge.

Nothing about existing behaviour changes. A run with no observer attached must
take exactly the path it takes today.

## What this is not

This is **dynamic** timing analysis — delays measured by simulating, not
computed from a delay model. Static timing analysis, which derives arrival times
from the graph without simulating, is faster and is what an optimiser would call
in a loop. It also depends on a delay model that would need validating against
exactly the measurements this produces. Dynamic first; static later, checked
against this.

## Out of scope

- Inserting delay to balance paths. That is the pass this enables, not this.
- Changing what delay `compile()` assigns to repeaters.
- Any layout change.
