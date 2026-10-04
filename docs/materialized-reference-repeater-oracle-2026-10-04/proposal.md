# Materialised reference route oracle proposal

Isolated clean source b01ad8e2ff92b60b4d3c1ed6c224f6c8aefaf72d. No production source or shared test has been changed. The patch changes only the all-pinned-full-adder test's comparison oracle; the entire legacy_lay_net_reference generator and its original counts remain unchanged.

The original frozen exact-route comparison fails at g21: actual count 2, frozen planned count 1. Its physical anchors, cells, floors, branch paths, ownership, sink and terminal kind are identical. RouteTerminal.repeaters explicitly promises every source-to-sink repeater, not only the body or the pre-terminal plan. Restoring 1 would violate that public contract and the final materialised-owner count.

The proposed oracle retains exact checks of every physical Route field and every terminal sink/kind. For each branch it independently indexes the reference's own materialised states by reference anchor and reads the reference branch path. It rejects missing/duplicate physical cells and compares the actual repeater count to that independent measurement. It calls no production counting helper and consumes no new-router decisions.

Positive qualification passes every route/branch in this fixture. Two historical omissions remain explicitly captured: g21 branch0 has frozen1, measured2, actual2; g7 branch0 has frozen3, measured4, actual4. Both old counts and zero mutations are rejected through the same full comparison oracle: four independent negative checks. Catch-unwind panic lines in the passing log are these deliberate negative controls, identified by FROZEN_COUNT_NEGATIVE records; no fixture or world golden is repinned.

The patch also asserts route-list lengths, preventing zip from silently ignoring an unmatched route. All other historical guards remain unchanged and failing in the original full-library audit. This proposal does not claim full-library closure.

Raw/compat RouteTerminal.repeaters represents the full path total. Expanded candidate validation/adaptation separately excludes a delivery OutputTerminalRepeater and absorbed target primitive from its propagation-owner count; its timing graph reads owned component states directly. This existing topology conversion is not a reason to reinterpret the raw field as body-only or to equate the frozen1 with raw materialised2.

Qualification command:

```
source /home/seith/.cargo/env
ulimit -s 524288
CARGO_INCREMENTAL=0 RUST_MIN_STACK=536870912 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/reda-measured-route-parity-target-b01ad8e cargo test --manifest-path /tmp/reda-measured-route-parity-clean-b01ad8e/Cargo.toml --lib compile::planner::tests::legacy_adapter_keeps_the_all_pinned_full_adder_routes_byte_exact -- --exact --nocapture
```

Terminal0: 1 test passed, 0 failed, 0 ignored; 1265 filtered; runtime3.76s. Only the clean manifest qualification is included; no dirty-workspace build establishes a conclusion.
