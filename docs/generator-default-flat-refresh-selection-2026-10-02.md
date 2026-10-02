# Default flat-refresh selection for larger boundaries

The default `compile()` now considers the structural flat-refresh correction outside the existing two-input/three-output family. It selects the correction only after strict certification and complete retained-history qualification against the independent original compilation. Eligibility is combinational, one through six inputs, and at least one public output. All public outputs are checked; the output width is not truncated. The six-input ceiling bounds exhaustive qualification at 8192 setup/target phases. This is not yet scalable decomposition or a general candidate search budget.

## Root cause and method

The previous qualifier encoded two inputs and three outputs in its loops, observation labels and fixed arrays. That prevented the certified full seven-segment correction from being selected using the same quality contract. The qualification body now derives addresses and observation width from the complete source boundary. The previous fixed-width public callback remains a wrapper, preserving consumers of the small-family protocol.

Qualification checks every implemented signal and every source driver/lamp after each phase. It retains simulator history and clears only observer records. Both setup and target compare their twelve existing metrics independently. A selected correction must be no worse on all twenty-four metrics. A tie is allowed for this structural certification repair; it is not reported as a delay or glitch improvement. The small-family policy retains its previous strict-improvement rule and tie preference.

The original producer remains `compile_without_rewrite()`. Only the selector's private baseline measurement can read an unsealed original world; public qualification still requires a sealed rewrite. Original controls are never admitted as certified candidates. Failed correction, failed certification, failed truth, or regressed quality retains the original producer outside the small family. DFF and boundaries above six inputs keep their original compilation path.

## Independent evidence

Actual-default probes for Native and fresh Yosys seven segment were independently audited against the frozen controls from the preceding flat-refresh experiment. Each record includes source and implemented graphs, every internal value, all seven drivers and lamps, full events and ticks, and full world bounds/BlockState fingerprints. All 512 phases match the previously certified candidate exactly. The Python audit independently reconstructs truth and setup/target metrics; neither frontend regresses any metric.

Native target worst settle remains 44 ticks, total 8823, setup total 8786. The larger quality objective therefore remains open. The previously rejected shared-cover candidates remain rejected.

The hand-written full seven-segment reference also selects the certified correction and passes complete qualification. Its independent original path remains Legacy. The reference path test checks both producers rather than replacing the original expectation.

The existing 4096-function family was regenerated three times through the final default entry. All bytes match each other and the preceding frozen default family (`e91fb1bb40a166035c58d2202cbf726d400d873e24a974c789eee8cad0a68b20`). Its 27 improvements, 4069 ties, and zero regressions against the independent certified control remain unchanged. All 2744 prior caps pass. Independent Python recomputation matches all twelve Rust setup and target metrics for every function; setup regressions and glitches are zero. The identical family artifact is reused rather than duplicated.

The boundary regression also checks one, three, and six inputs through 8, 128, and 8192 phases, four public outputs including two constants, independently specified truth, exact protocol order, and rejection of unsealed public qualification. Two flat-refresh/boundary tests, three selector tests, six physical-view tests, and the wide-merge, DFF power-on, DFF edge/hold, and original/selected reference-path tests pass.

## Regression investigation and remaining work

The old reference size test remains failing: and4 measures 238 versus its historical 232 expectation; full_adder measures 1017 versus historical 1065. Independent original and selected producers have identical worlds for both circuits, establishing that this drift predates the new correction selector. Their expectations have not been changed. Sizes are diagnostic historical snapshots, not quality acceptance. These failures and the prior root-library failures still require root-cause resolution under the full-project objective.

This round does not establish larger-ROM shared generation, improved large-circuit timing, useful search budgets, new sequential/HDL support, or real Minecraft conformance. The eight-item goal stays active. Exact artifacts and completed repeat/regression counts are recorded in the accompanying summary.

## Reproduction

```sh
source tools/env.sh
export RUST_MIN_STACK=134217728
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 0 --default > /tmp/default-seven.jsonl
# Decompress the preceding frozen native-control JSONL into /tmp/control.jsonl.
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/default-seven.jsonl
cargo test --release --test flat_refresh --test physical_rewrite_views --test dff_physical --test m3_m4_dff_yosys_differential --test compile_wide_merge
cargo test --release --test reference_circuits every_reference_circuit_records_which_path_produced_it
```

Add `--yosys` only for the optional fresh oracle probe. Native generation itself does not invoke Yosys. This round supplies simulator and strict certification evidence, not game-server results.
