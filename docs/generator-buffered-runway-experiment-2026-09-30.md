# Buffered terminal runway experiment (rejected)

Control: `00448fb`; production source is identical to `bcf6cb8`. The prior rejected buffered-dust integration validated straight relay tails after search and refused `[5,3,2,0]`. This iteration reapplies that archived experiment and imposes a two-cell sink runway inside `search_path` whenever a sink requests `BufferedDust`. Any caller's longer runway is preserved. The buffered landing still requires a private straight horizontal predecessor relay, its recorded floor/direction/count, and its side guard.

The new complete patch is archived as `generator-buffered-runway-rejected-2026-09-30.patch`. It is withdrawn from production.

The search contract resolves the previous refusal on words `[5,3,2,0]`. The actual stock plan and emitted route ledger have identical canonical world fingerprints, and all 32 setup/target history phases have correct internal gate drivers and output lamps. The observed buffered NOR terminal tails are straight, and the final support landing remains dust.

Two other known defects still cannot obtain a candidate route ledger:

| ROM words | Reported local route endpoints |
| --- | --- |
| `[4,1,0,2]` | `(39,1,33)` to `(40,1,29)` |
| `[4,3,2,5]` | `(35,1,40)` to `(37,1,41)` |

Both diagnostics report `planner=Legacy` from stock compile, followed by `NoLocalRoute` from `plan_from_netlist`. This fallback does not demonstrate the buffered candidate's physical validity. No successful buffered history is claimed for either table. Requiring the relay access suffix during path search fixes one late refusal but does not provide the required placement space and reservations in every existing geometry.

Independent quality suites also reject the candidate:

| Circuit | Metric | Experiment | Frozen cap |
| --- | --- | ---: | ---: |
| and4 / REDA | Worst output change ticks | 18 | 16 |
| xor2 / native | Lamp worst change ticks | 16 | 14 |

The AND measurement has worst world settling 18 ticks, mean 8.22265625 ticks, 13 output glitch transitions, and zero single-input glitches. Its 18-tick output is better than the previous rejected experiment's 20 ticks, but that is not the acceptance baseline: the independent 16-tick cap remains authoritative. Fresh-Yosys success and full-family qualification are not claimed.

Both modified source files are restored exactly to the control. No accepted tests, caps, input holds, simulator rules, or fallback acceptance criteria are relaxed. Automatic selection remains disabled.

Next architectural requirement: represent the relay's physical access space in placement/primitive geometry before routing, and evaluate the resulting whole-circuit candidate against independent driver/lamp controls. A routing suffix alone cannot establish either physical feasibility or acceptable total latency. Candidate refusal must remain visible rather than being treated as successful optimization through a legacy fallback.
