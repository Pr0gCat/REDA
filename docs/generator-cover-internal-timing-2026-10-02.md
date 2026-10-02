# Internal waveform investigation of larger shared covers

No new cover is accepted and shipping selection is unchanged. This round adds opt-in internal event tracing to the existing physical cover probe, audits the retained internal trajectories independently, and tests two structural reductions against unchanged original-world controls. Neither reduction meets the full quality contract.

## Observation and mechanism

The previous probe checked every internal gate at phase completion but retained events only for the public drivers and lamps. `--trace` now also watches every implemented gate output using the existing observer. Internal events are recorded separately, with actual gate positions and implemented output bindings. Public events keep their previous schema and order. No simulator or verification rules, input protocol, settle limit, or history handling change.

For the Native original implementation at target 0 to 14, g5 briefly rises at tick12 and falls at14. g9 rises at18 and falls at20; later g10 falls at28 and rises at30. g19 falls at26, rises at28 and falls at38; g31 rises at20 and falls at40. The final lamps settle at44. These are measured internal transitions, not inferred logical gate costs.

In the original shared positive cover at target 2 to 8, the three-input cube `$cover_gate_7` has direct inputs d2/d1/d0. It rises at8 and falls at10 although its expected start and end value are both false. This demonstrates a transient all-zero condition reaching the cube during a multiple-input switch. The pulse propagates through the output reduction network. Source-equivalence and steady truth pass; they do not eliminate this physical arrival skew. Matching complementary rails and branch arrivals therefore remains necessary. This evidence does not yet identify the exact landing-cell arrival of each operand; that is the next measurement needed before proposing physical balancing.

## Structural experiments

The cover builder also introduces avoidable reduction stages. Above three inputs it builds a complete OR tree followed by a unary NOR, rather than making the final stage NOR. Its three-way chunks even build a unary merge for a singleton remainder. `--fused-reductions` tests final-stage NOR and direct singleton forwarding. `--prune-unused` additionally removes gates outside the declared output cones; this removes two unused constant-producing gates in these covers. Both methods use structure rather than circuit names or function IDs. They are experimental switches, not shipping compiler optimizations.

| Native candidate | Worst target ticks | Lamp glitch transitions /256 |
|---|---:|---:|
| Fixed original control | 44 | 85 |
| Previous positive shared cover | 46 | 117 |
| Fused positive cover | 48 | 100 |
| Fused and pruned positive cover | 46 | 115 |
| Previous mixed-polarity cover | 66 | 120 |
| Fused mixed-polarity cover | 52 | 111 |
| Fused and pruned mixed-polarity cover | 58 | 118 |

Fused mixed polarity improves over its previously rejected cover, but still regresses against the original control. Every new cover has multiple setup/target regressions. Removing more logical stages also changes physical placement/routing and can worsen measured timing. All four optimized covers are rejected; no reduced gate/block count is an acceptance reason. The original baseline record remains accepted with unchanged 44-tick quality.

## Evidence and validation

Each fused and fused/pruned experiment contains the original certified baseline and two covers. All three records pass strict certification, source/implementation truth, all internal values and all seven drivers/lamps across the complete 512-phase retained protocol. Each experiment is regenerated three times with identical full bytes, including internal/public events, world bounds and full BlockState fingerprints. Compressed records and every setup/target comparison are in the accompanying `-fused.jsonl.gz`, `-pruned.jsonl.gz`, and `-summary.json`.

`tools/compare_cover_probe.py` independently evaluates the recorded source/implementation graphs and reconstructs each gate's retained state from its internal events. It validates signal identity, event time bounds/order, actual transitions and agreement with the final observed and logical state. A runnable standard-library regression rejects deleted, duplicate, unknown-signal and out-of-range events while accepting the real baseline and rejecting the real regressed cover.

The unflagged producer was replayed against the preceding frozen candidate artifact: all records are identical. The original trace without new experiments also reproduces the same worlds, public events, internal final states and quality when its diagnostic fields are removed. Frozen controls have not been rewritten. This experiment uses Native generation without Yosys; it is not a real Minecraft conformance result.

## Reproduction

```sh
source tools/env.sh
export RUST_MIN_STACK=134217728
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 2 --flat-refresh --trace --fused-reductions > /tmp/fused.jsonl
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 2 --flat-refresh --trace --fused-reductions --prune-unused > /tmp/pruned.jsonl
# Decompress the unchanged preceding native-control artifact into /tmp/control.jsonl.
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/fused.jsonl
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/pruned.jsonl
python3 -m unittest discover -s tools -p 'test_*.py'
```

The full eight-item goal remains open. In particular, this is diagnostic progress toward actual large-circuit timing improvement, not successful shared-decoder replacement or scalable structural composition/search.
