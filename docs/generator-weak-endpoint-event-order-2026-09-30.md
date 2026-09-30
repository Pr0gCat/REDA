# Endpoint event order and torch recovery

Control: `dac30ea`. This iteration extends only the manual endpoint observer, recording the gate, repeater/dust cell before the landing, landing dust/repeater, and the other input dust. It includes setup phases, target phases, relative event ticks, and absolute phase start ticks. Instrumentation must reproduce the independently frozen original output event summary exactly. Neither endpoint geometry, simulator behavior, nor production generator changes.

The raw data is `generator-weak-endpoint-events-2026-09-30.json`. Both independent runs produce identical complete phase records, and the original frozen output traces are unchanged.

## New pulse mechanism

For target `2 -> 1` (b falls, a rises), the weak arm's b landing falls at tick 1. Its a repeater and a landing dust become high at tick 2, but the torch also becomes high at tick 2, then low at tick 4. The direct-support control's repeater becomes high at tick 2 and its torch stays off. This isolates the difference to the intervening dust cell, not different configured repeater delay.

In the current simulator, `advance_one_tick` applies the entire batch of due component events before recomputing dust strengths. The repeater update can therefore affect direct strong support power immediately, while its dust landing is still stale when the due torch event reads support power. Only after those component events are applied is the landing dust recomputed. A subsequent torch correction is scheduled two ticks later. The bounded path uses the same batch/recompute ordering. This is a verified explanation of the current simulator's pulse, not certification of Minecraft's update ordering. Simulator rules are preserved; changing them to make a candidate pass is not part of this experiment.

## Long recovery mechanism

Before weak-arm target `2 -> 0`, the prior observed torch changes are at absolute ticks `[4,8,10,12,14,18,22,26,28]`. At the first due on event (absolute tick 32), all nine lie inside the 60-tick window, exceeding the simulator's eight-change limit. At absolute tick 64, the first change at tick 4 exits the strict sliding window, leaving eight; the torch lights at exactly tick 64. The phase begins at tick 30, hence its 34-tick recovery. These are the observed normal flips leading to the first recovery, not counts inferred from the final settled state.

The control instead has seven prior changes `[4,8,10,12,14,18,22]`, and lights at its first due event, absolute tick 30 (relative tick 2). `apply_torch_tick` records normal flips and suppresses changes while burned out; `is_burned_out` counts changes strictly newer than `now - 60` and requires more than eight. The measured recovery agrees exactly with these rules. The extra candidate pulses can therefore affect later transitions through retained torch history.

The original timing-preservation hypothesis remains rejected. A future endpoint needs to preserve event ordering as well as the configured delay, and must be assessed with retained history. No observer reset of physical torch history, longer input holds, relaxed caps, or automatic selection is introduced.
