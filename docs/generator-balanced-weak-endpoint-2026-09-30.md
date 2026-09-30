# Balanced weak-support endpoint prototype

Control: the independent direct-repeater NOR endpoint frozen in `dac30ea` and expanded in `6860221`. The control's geometry, all output traces, and all 32 complete phase records remain exactly unchanged. This iteration changes the candidate only: a delay-1 repeater is placed on each input branch before the final dust landing, so both input changes cross the same component/dust update stages. The support receives dust rather than direct repeater power.

The standalone probe's `balanced` mode checks NOR output truth in all 16 setup/target pairs per arm, and independently checks both candidate landing wires against their own input levers. The control still demonstrates eight backfed phases. Candidate final input-wire truth is correct throughout. No reset of physical torch history or extra input hold is introduced.

| Local metric | Frozen control | Balanced candidate |
| --- | ---: | ---: |
| Worst world settling ticks | 26 | 12 |
| Total target settling ticks | 48 | 44 |
| Worst driver change ticks | 26 | 12 |
| Driver glitch transitions | 0 | 0 |
| Single-input driver glitch transitions | 0 | 0 |

The probe requires every listed local metric to be no worse than the independent control. Neither component counts nor occupied volume determine acceptance. Raw observations, including every setup phase, are in `generator-balanced-weak-endpoint-2026-09-30.json`. The original asymmetric weak candidate and its rejected traces remain frozen separately. Default probe mode still requires exact agreement with that original independent output summary.

The simultaneous `2 -> 1` transition no longer creates the asymmetric candidate's two-tick pulse. Aligning both input update stages addresses the observed event-order mismatch. The remaining worst 12-tick settling shows that retained torch recovery still matters; this is not a zero-recovery claim. Four target traces differ from the control: three formerly two-tick changes take four ticks, while `3 -> 0` improves from 26 to 12 ticks. Local aggregate improvement therefore does not prove every individual transition is faster.

This is a successful local physical prototype, not a shipping generator improvement, typed primitive certification, complete stock ROM qualification, or Minecraft certification. There is no output lamp in this isolated endpoint fixture, so project-wide nine-metric driver/lamp qualification is not claimed. The production generator, simulator rules, accepted quality caps, and automated stock tests remain unchanged. The optimization goal remains active.

Next integration gate: model the repeater-before-dust landing in real routing metadata, certify emitted ownership/strength/direction and actual output boundaries, then run complete histories and the existing independent production timing/glitch controls. Do not replace the stock strategy based only on this fixture.

Reproduce:

```
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example weak_support_endpoint_probe -- balanced
```
