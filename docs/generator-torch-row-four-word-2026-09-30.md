# Four-word torch matrix with physical literal decoder

Status: improved physical prototype on a fixed reference; not integrated into
`compile` and not an accepted production generator optimization.

## Problem and causes

A ROM emitted as separately routed gates repeats selection logic and exposes
path skew. The proposed general physical matrix has upper true/inverse literal
rails, mismatch torches feeding each word-selection rail, programmed side
torches on that rail, and shared output dust below. Two actual address levers
and physical inverters drive all literals; there are no independent row-enable
inputs or truth-table substitutions for the decoder.

Raw one-row-per-address selection leaves gaps on same-value output transitions.
Merging rows using the existing frontend cover algorithm retains implicants and
single-input consensus, but direct and inverted literal phases still differ.
A general phase correction gives true-rail repeaters delay 2 and inverted-rail
repeaters delay 1, compensating the inverter's 2 game ticks. No case names are
used in the builder or phase correction.

A reordered cover also exposed a genuinely unpowered last inverse-rail cell:
without restoration, the extra source ascent exhausts strength before the far
row. All literal rails now restore before fanout; longer variants receive
another restoration stage. Output columns also restore when their run exceeds
the tested strength bound. These stages are physical blocks; the simulator is
unchanged. The longer variants still have multi-input timing hazards.

## Independent reference and experiment

Data words are [1,6,3,5] in address a + 2*b order. Existing compile/lowering
produces y0=~a|b, y1=a^b, y2=a. Unlike the previous constant-column reference,
all three reference outputs are entirely compiler-generated. Candidate and
reference both carry three output lamps. Every final driver and lamp value is
checked against literal data words. All 16 transitions use persistent history,
with 16 setup transitions, no holds or cooldown, and end-of-game-tick probes.

| Variant | Worst world ticks | Total world ticks | Driver glitches | Single-input driver glitches | Lamp glitches | Worst driver change | Worst wrong-target pulse | Total wrong-target pulse |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Frozen existing generator | 20 | 187 | 2 | 0 | 2 | 16 | 4 | 8 |
| Restored raw word rows | 14 | 150 | 7 | 3 | 1 | 10 | 2 | 14 |
| Shared implicant rows | 14 | 151 | 4 | 0 | 1 | 10 | 2 | 8 |
| Shared rows plus phase alignment | 14 | 162 | 0 | 0 | 0 | 10 | 0 | 0 |

The first two candidates regress driver glitches and are diagnostic variants,
not accepted alternatives. Phase-aligned rows improve every measured aggregate
or hold it equal. Mean world settling is 11.6875 -> 10.125 game ticks. Gate and
block counts are not quality criteria. The reference's nine scalar metrics are
frozen independently of candidates. Full traces accompany this report.

## Exhaustive physical qualification

All 4096 two-address-bit, three-output-bit data tables were generated twice and
canonical fingerprints matched. Each underwent all 16 measured transitions and
16 setup transitions: 65536 measured plus 65536 setup transitions. Every settled
driver and lamp matches its independent data word. Across the entire family:
maximum settling 18 game ticks, zero single-input driver glitches, 48 multi-input
driver glitches. These are prototype diagnostic caps, not replacements for any
existing production quality threshold. There is no full existing-generator
quality comparison for all 4096 tables, and the family is not claimed uniformly
faster or pulse-free.

The builder reuses `src/frontend/cover.rs` via a test module; its existing exact
three-input cover test also runs. All builders use deterministic ordered rows.
Validation: `cargo test --release --test torch_row --test torch_row_decoder --
--nocapture`: 3 row tests plus 4 decoder/cover tests passed. Exhaustive replay
completed in 31.86 seconds. Final fixed-reference replay after adding all nine
quality assertions passed. Existing packing.rs unused_mut warning persists.

## Integration constraints and next work

`CompiledCircuit` and consumers correlate gate-output positions and physical
verification to the supplied netlist. A truth-table matrix cannot silently
collapse that netlist or bypass the existing verifier pipeline. Production
integration needs a truthful macro representation and mapping, alongside
physical strength/coupling invariants. The test-only builders change no compiler
or frontend code, no defaults, no DFF behavior, and no quality baselines.

Before integration, investigate the 48 multi-input hazards and longer-matrix
stage skew, preserve this fixed reference and the existing main/matrix timing
caps, then validate production generation and appropriate regressions. These
simulations are not Minecraft runtime certification or maximum-clock results.
