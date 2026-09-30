# Uniform weak-support drive experiment (rejected)

Control: `0aee9b6`. The typed router already supports `TerminalRequirement::DirectedDust`, and its branch budget explicitly prevents counting a terminal refresh when that requirement would overwrite the terminal with dust. This permits a real routing experiment, rather than rewriting blocks after emission.

Experiment: require directed dust at non-merge gate inputs in ordinary combinational circuits. Sequential circuits and routes serving pinned terminals retain their existing policy. Merge and external terminal contracts remain unchanged. The intent is to remove strongly powered NOR supports, preventing support power from returning into another input's dust. This changes physical landing requirements only; no module names, ROM values, or block/gate-count quality criteria are inspected.

Physical diagnostics pass for all three known fanout defects: `[5,3,2,0]`, `[4,1,0,2]`, and `[4,3,2,5]`. Every internal driver and output lamp is correct across all 32 setup/target phases per case (96 total). Each diagnostic checks exact world equality between the emitted route ledger and stock compile.

The fixed quality controls reject the experiment:

| Circuit | Metric | Experiment | Frozen cap |
| --- | --- | ---: | ---: |
| and4 / fresh Yosys | Output glitch transitions | 9 | 0 |
| majority3 / native | Lamp glitch transitions | 1 | 0 |

The AND result has worst settling 26 ticks, output change 12 ticks, mean settling 5.48046875 ticks, and zero single-input glitches. Faster or otherwise acceptable timing does not compensate for exceeding a glitch cap. The independently frozen output timing baseline remains unchanged. Both quality suites were run independently; neither rejection was hidden by the other's early exit.

The terminal-policy change is withdrawn, and production source is restored exactly to the control. No accepted tests, diode assertions, metric caps, physical histories, or observation durations are weakened. A full-family qualification is not claimed. Automatic torch-row selection remains disabled.

Next experiment: preserve an existing terminal repeater's delay while moving its strong output to an intervening dust cell, so the NOR support is weakly driven. Uniform weak landing by itself changes branch delay/refresh decisions and is not an acceptable replacement. A replacement must be evaluated as a typed physical terminal with exact metadata and per-transition timing/glitch verification before deployment.
