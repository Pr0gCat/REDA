# Required refresh versus discretionary endpoint stages (rejected)

Reference: 05d09e3. The previous directional footprint could route the intended
weak endpoint, but moving a discretionary relay still gave one Mux input a
two-tick head start. This experiment changes internal weak arrivals to directed
dust rather than copying Automatic's repeater choice. The branch strength model
cannot rely on a final repeater being kept: it must place any necessary refresh
upstream or refuse the route. A one-cell straight landing is required; publicly
observed source nets retain their private relocated diode via WeakRepeater.
Merge and sequential terminal policies are preserved. The directional footprint
from the preceding experiment remains part of this candidate. Connectivity,
terminal contracts and strength determine the rule, not names or case numbers.

Both Mux and [5,3,2,0] ROM obtain exact Unified3d ledger/emitted-world agreement
and retain correct internal driver/output lamp truth through every history
phase. Mux has 64 setup plus 64 target phases; ROM has 16 setup plus 16 target
phases. The Mux two extra pulses at 2->4 and 2->5 disappear. Both upstream
inverters still change at tick 2, with no discretionary relay to separate them.
Repeated full Mux stdout and world fingerprints match exactly.

| Mux quality | Independent stock | Candidate |
| --- | ---: | ---: |
| Worst world settling | 12 | 10 |
| Total target settling | 400 | 364 |
| Driver worst change | 8 | 6 |
| Lamp worst change | 12 | 10 |
| Driver glitches | 6 | 6 |
| Lamp glitches | 4 | 2 |
| Driver total pulses | 16 | 12 |
| Lamp total pulses | 12 | 10 |
| Driver longest pulse | 4 | 2 |

Lamp longest pulse remains 5; single-input driver/lamp glitches remain 1/0.
The current stock Mux phases and fingerprint match the separately frozen
previous control exactly. This local improvement is not sufficient to ship the
policy across other circuit shapes.

The independent suites both reject it. xor2/native has lamp total pulse ticks
8 > frozen cap 6; compared with the stricter current checkpoint, its driver and
lamp glitches are 2/2 rather than 1/0. Candidate driver/lamp longest pulses are
4/7 and worst changes 8/12. The fresh-Yosys AND4 output glitches are 9 > 0;
its reported world worst settling is 26, worst output change 12, mean settling
5.48046875 and zero single-input output glitches. The suite stops on its
unchanged glitch assertion; no complete candidate-suite pass is claimed.

## Necessary refresh produces a different arrival imbalance

The diagnostic now also supports and4-yosys, generated freshly from the same
fixture. It watches every gate driver and routed repeater and independently
checks conjunction output truth across 256 setup plus 256 target phases.
Candidate and restored stock compile twice deterministically and their full
observations repeat identically. Their world fingerprints match the respective
fixed-suite worlds. Input histories are explicit: this diagnostic uses a,b,c,d
from least-significant to most-significant bits; the unchanged frontend suite
uses a,b,c,d from most-significant to least-significant. Its 9-glitch count is
from that original fixed suite, not inferred from a differently ordered history.
Both full records are retained with the input convention named in JSON.

In diagnostic target 3->12 (a,b turn off; c,d turn on), both merge drivers n2
and n5 change at tick 4. The n2 route has no relay, while n5's longer path has
a required refresh at (46,1,28), changing at tick 6. Candidate g2 rises at 6
and falls at 8; its lamp rises at 7 and falls at 12. Stock instead has both
receiver input relays change at 6 and no g2/lamp pulse. Thus deleting optional
stages fixes the Mux mismatch but does not balance actual strength-driven
stages. The next general timing model must consider both upstream gate delay
and actual routed refreshes at each receiving gate, and still prove all quality
caps. Universal added buffering was already rejected; it is not accepted by
this diagnostic evidence.

The entire generator/layout/router experiment is archived and withdrawn.
planner.rs, routing.rs and relax/build.rs are byte-identical to HEAD. The
unchanged frontend and generator quality suites pass all 1312 measured target
transitions after rollback. git diff --check passes. Only the diagnostic example
extension and evidence remain as new changes. No test cap, observation interval,
simulator rule, physical history, or independent control is weakened. Automatic
ROM-row selection is still disabled and the broader optimization goal remains
active; no full repository suite or Minecraft certification is claimed.

Reproduce after source tools/env.sh, with RUST_MIN_STACK=33554432:

    cargo run --release --example mux_route_timing_diagnostic
    cargo run --release --example mux_route_timing_diagnostic -- and4-yosys
    cargo test --release --test frontend_quality_report --test generator_quality_matrix

The failed patch belongs in an isolated experimental checkout; it is not shipping
source. Fresh Yosys diagnostics require tools/env.sh's environment.
