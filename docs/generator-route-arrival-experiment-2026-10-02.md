# Route arrivals and primary-condition balancing experiment

This round measures actual gate-input route arrivals and verifies a specific cause of an internal cube pulse. It does not accept a new optimization or change shipping selection. Reversing input placement and two balancing proposals fail the existing full quality contract; one terminal-conversion candidate also fails internal truth.

## Measurement contract

`rom_cover_search --trace-landings` reads the actual sealed implementation manifest. It records every external gate-input branch's terminal, plus its last interior repeater when present. Metadata includes typed route/sink identity, receiver/input index, source signal, actual positions, block states and branch path. Gate event tracing remains enabled. All public drivers/lamps and every implemented gate retain their existing exhaustive truth checks.

A terminal dust's positive strength is not necessarily its source's Boolean value: the terminal can be powered from the receiving neighborhood. The last repeater provides a separate directional observation. The initial failed attempt to equate every terminal dust with its own source was an invalid observation assumption, not a compiler or simulation contract. It was replaced by observing both physical sites. No verifier or simulator rule changed. All recorded repeater sites agree with their logical source in every complete phase; terminal dust differences remain recorded rather than hidden.

The independent audit validates every receiver/input-index/source binding against the implemented graph and requires complete gate-input coverage. It reconstructs all gate and landing states from the retained events, rejects unknown/duplicate/out-of-bounds events and missing operand observations, and independently checks directional repeater values against the source. Public quality continues to be derived only from the existing public event protocol.

Coverage per original Native record: 74 operand pairs for the original implementation, 100 for the positive cover, and 109 for mixed polarity. Every pair is observed over all 512 phases. Last interior repeater observations have zero settled source disagreements. Original terminal observations differ from their own source in 1408 NOR-input samples and 832 merge-input samples; this is precisely why terminal wire state cannot substitute for the isolated source rail.

## Verified arrival cause

At positive-cover target 2 to 8, `$cover_gate_7` reads d2/d1/d0. Their paths contain 3/3/4 repeaters. d2 falls at tick6 at both the last repeater and terminal; d0 rises at tick8 at both sites. d1 remains false. The resulting two-tick all-zero input interval makes the NOR cube rise at8 and fall at10.

`--balance-primary` considers only multi-input NOR gates whose operands are primary inputs, starting from unit-delay routes. It estimates arrival from complete branch refresh counts and adds the needed delay at an existing route-owned interior repeater. The cell must belong to only that branch, be outside observations and the terminal, and stay within Minecraft's four-delay setting. All placements, cells, facing and connection geometry remain unchanged. Whole source-equivalence, physical certification and retained-waveform qualification are still mandatory.

The measured d2 fall moves to tick8, matching d0's rise; the cube has no event for this transition. This experimentally verifies the local arrival-skew cause. It does not establish a global win: other transitions settle later.

## Rejected alternatives and complete quality

`--reverse-inputs` tests reversed physical input placement on the fused/pruned covers. Source order remains unchanged; the rewrite certificate binds implementation inputs explicitly in source order. The observer and independent evaluator drive/read that binding rather than reinterpreting address bits in physical declaration order.

`--balance-terminal` tests adding delay at the terminal instead. It can change dust to a repeater and updates owned block state, terminal style, delay ownership and refresh count before full certification. This changes local electrical strength, unlike modifying the existing interior relay. The mixed-polarity version passes source/physical certification but yields an incorrect internal `$cover_gate_19` at from0/to2, target address2 (expected true, actual false). It is rejected immediately; qualification errors are now serialized so one failed experiment cannot discard the other records or appear accepted. No extra hold or history reset is used to repair it.

| Proposal | Worst target | Target total | Setup total | Lamp glitch transitions /256 |
|---|---:|---:|---:|---:|
| Fixed original Native control | 44 | 8823 | 8786 | 85 |
| Original implementation, interior balancing | 44 | 8829 | 8792 | 85 |
| Original implementation, terminal balancing | 44 | 8830 | 8793 | 80 |
| Positive cover, interior balancing | 46 | 9057 | 9019 | 102 |
| Mixed cover, interior balancing | 66 | 11200 | 11151 | 112 |
| Positive cover, terminal balancing | 46 | 9074 | 9036 | 97 |
| Mixed cover, terminal balancing | Internal truth failure | — | — | — |
| Fused/pruned positive cover, reversed inputs | 46 | 9332 | 9291 | 124 |
| Fused/pruned mixed cover, reversed inputs | 48 | 9643 | 9606 | 117 |

The original implementation's interior balancing reduces driver/lamp total wrong-pulse duration from 1520/1425 to 1512/1417 ticks, but increases both total settle metrics by6. Terminal balancing reduces some glitches further but also increases both totals. Every modified candidate is rejected against the fixed original control, including ones that are better than previously rejected covers. This supports shortening late paths before adding delay to early paths. It is not a gate/block-count acceptance decision.

## Reproducibility and checks

The original route trace and each of the three modified experiments were generated three times, each with identical full JSONL bytes. Frozen compressed artifacts and all metrics, coverage and hashes are in `generator-route-arrival-experiment-2026-10-02-{landings,reverse-landings,balanced-interior,balanced-terminal}.jsonl.gz` and `-summary.json`. The terminal experiment preserves the two complete records and explicit truth failure; it does not claim complete truth for the failed record.

Removing diagnostic fields from the unmodified landing trace yields the preceding frozen candidate records exactly, including complete public events, internal final values, worlds and fingerprints. The unflagged producer also replays those records exactly. Frozen controls are unchanged. The standard-library audit tests cover real accepted/regressed records, corrupted internal/landing events, source bindings, missing operand coverage and rejection of the real internal-truth failure.

```sh
source tools/env.sh
export RUST_MIN_STACK=134217728
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 2 --flat-refresh --trace-landings > /tmp/landings.jsonl
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 2 --flat-refresh --trace-landings --balance-primary > /tmp/balanced.jsonl
# Decompress the unchanged preceding native-control artifact into /tmp/control.jsonl.
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/landings.jsonl
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/balanced.jsonl
python3 -m unittest discover -s tools -p 'test_*.py'
```

Add `--balance-terminal` instead of `--balance-primary` for the rejected electrical conversion. Add `--fused-reductions --prune-unused --reverse-inputs` for placement reversal. These are diagnostic switches, not compiler defaults. Native generation uses no Yosys in this experiment. Real Minecraft conformance and the full eight-item objective remain incomplete.
