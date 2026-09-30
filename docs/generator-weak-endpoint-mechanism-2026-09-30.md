# Repeater-before-dust endpoint mechanism probe

Control for this isolated physical experiment is an independent hand-built two-input NOR endpoint, not the stock compiler. Its support is `(5,1,5)`, its north-facing torch is `(5,1,4)`, and its inputs arrive from west and east. The west input has one delay-1 repeater. The east input uses dust. In the control, the repeater stands at `(4,1,5)` and strongly powers the support. In the candidate, it moves to `(3,1,5)` and the former repeater cell becomes dust. Both arms retain one repeater; component count is not an acceptance metric.

`examples/weak_support_endpoint_probe.rs` constructs both worlds independently, applies the identical complete repeated from/to history, checks NOR truth in every setup and target phase, observes exact torch events, and checks that the candidate's east-input wire always matches its independent lever. It also requires the control to actually demonstrate backfeed. This is a physical mechanism probe, not stock-planner certification or Minecraft qualification.

The frozen result is `generator-weak-endpoint-comparison-2026-09-30.json`:

- Both arms have correct final NOR truth across 16 target transitions and 16 setup phases each.
- The control repowers the false east-input wire in eight phases; the candidate has no such wire truth error.
- Three of 16 target event traces differ. At `2 -> 0`, the control torch turns on after 2 ticks, while the candidate turns on after 34. At `2 -> 1`, the control has no torch event, while the candidate turns on at tick 2 and off at tick 4: a new two-tick pulse. At `3 -> 0`, the control turns on at 26 ticks, while the candidate turns on at 4.

The endpoint therefore prevents reverse support power but does not preserve output latency or transient behavior. Equal repeater delay settings cannot establish equal circuit timing. The changed propagation path exposes a new pulse under simultaneous input changes; the exact contribution of torch recovery to the longer repeated-history timing is not established by these output traces alone. No timing reset, longer input hold, weakened check, or stock integration is used to hide the discrepancy.

The timing-preservation hypothesis is rejected. The diagnostic and raw event records are retained; production source remains unchanged at `2fe11cf`. A future endpoint must be assessed across complete histories, including pulses and recovery, before any router metadata or generator policy changes.

Reproduce:

```
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example weak_support_endpoint_probe
```
