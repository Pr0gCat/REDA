# Root library audit after route contract repairs

The full release library run completes with exit 101: 1,172 passed, 13 failed, 71 ignored, 659.02 seconds. The command is `RUST_MIN_STACK=134217728 cargo test --release --lib -- --nocapture` after sourcing `tools/env.sh` and setting the compiler stack to 65,536 KB. This is a library run; integration tests are not implied.

The run launches at `8faaa73` with separate unfinished arithmetic/growth/clock work present. Its source tree was not frozen as an isolated snapshot. The later strong-sibling fixture repair `84b3d75` is not in this binary; its targeted pass must not be subtracted to manufacture a newer full-root count. Complete raw output, artifact hashes and all 13 failure names are retained alongside this report.

The original adapter parity, routing-report control and public/control-entry contract repairs pass in this run. Remaining failures cover historical geometry/negotiation/strength/resettle records, a physical-inventory assertion predating comparator variants, the old pinned sibling prerequisite and the unfinished arithmetic growth probe.

The arithmetic growth probe previously passed but now refuses with `NoLocalRoute { from: (31,1,79), to: (71,1,86) }`. That is a new qualification failure to investigate, not evidence of arithmetic completion. Correctly transmitting decay geometry changes congestion charging and can alter a growth trajectory; an identical-source A/B is required before attributing this failure to that change. Earlier accepted counter worlds must remain protected. The live positive decay-envelope integration run has no complete result yet; both older negative runs remain live.

Perf sampling was attempted for a known negative test PID after sustained CPU activity was verified. The host rejects it because perf_event_paranoid is 4. No kernel setting or capability was changed. CPU activity and small RSS do not establish a search root cause.

All eight goal items remain incomplete. No test, simulator/verifier rule, quality threshold or historical snapshot is weakened to obtain this report.
