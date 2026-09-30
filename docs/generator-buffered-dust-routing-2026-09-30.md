# Buffered-dust routing integration experiment (rejected)

Control: `bcf6cb8`. The standalone balanced weak endpoint is locally useful, but has not qualified whole-circuit timing. This iteration attempted an actual typed router integration, rather than a post-emission block rewrite.

The experimental `BufferedDust` sink requirement keeps the final support landing as directed dust and inserts a delay-1 repeater at the immediately preceding private branch cell. It refuses tails shorter than three cells, shared repeater cells, vertical steps, and bends. The emitted route records the repeater block, floor, direction, repeater count, and side guard. Merge requirements remain unchanged. The planner requests the new requirement on ordinary combinational non-merge inputs; sequential circuits and routes serving pinned terminals retain the existing policy.

The exact rejected patch is archived as `generator-buffered-dust-rejected-2026-09-30.patch`, based on `bcf6cb8`. It is not applied to production source.

Two independent quality suites reject the experiment:

| Circuit | Metric | Experiment | Independent cap |
| --- | --- | ---: | ---: |
| and4 / REDA | Worst output change ticks | 20 | 16 |
| xor2 / native | Lamp worst change ticks | 16 | 14 |

The native AND experiment has worst world settling 20 ticks, mean 11.85546875 ticks, seven output glitch transitions, and zero single-input glitches. These other measurements cannot compensate for exceeding the output timing cap. The fresh-Yosys AND cap remains 12 ticks and is not relaxed; this experiment fails earlier on the native arm, so no fresh-Yosys success is claimed.

The `[5,3,2,0]` manual diagnostic cannot obtain a verified route ledger: `plan_from_netlist` refuses route owner `b` as `PhysicalInvariant`. Thus there is no claimed successful physical history for that integration case. Enforcing a straight private tail only after the path has been selected does not teach path search to reserve the required two-cell terminal geometry. The whole-circuit experiment also introduces additional buffered stages along logic paths; identical local input alignment does not establish acceptable total output latency.

The integration is rejected. Both production files are restored exactly to the control; the new public requirement variant, placement logic, and planner selection rule are removed. Existing tests, timing/glitch caps, and observer histories are unchanged. No full-family qualification or shipping optimization is claimed. The standalone prototype and its independent frozen controls remain available.

Next integration requirements: terminal geometry must constrain path search before a branch is accepted, and candidate choice must evaluate full-circuit timing and real output lamps. Unconditionally replacing every ordinary NOR landing is not justified by a locally improved endpoint. Do not silently accept a fallback circuit when the candidate cannot satisfy its physical contract.
