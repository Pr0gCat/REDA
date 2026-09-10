# Refresh Relocation Retention Report

## Pre-feature provenance

- Harness commit: `d4eb27d87414eebcd2bca4a5ed1e4fcb2e070412`
- Hierarchy transcript: `C:\Users\LTY\AppData\Local\Temp\reda-refresh-relocation-pre-feature.txt`
- Hierarchy transcript SHA-256: `1941978F2C128358AE81C642E22B6B406CFF0DEE5ED0A5FBE5767BF4F9A30301`
- Flat baseline: `C:\Users\LTY\AppData\Local\Temp\reda-refresh-relocation-flat-control-baseline.json`
- Flat baseline SHA-256: `F2AEC004D1C2D10D278D4880117985B8885F6C5137BCB316F09C79B8DCAB46D9`
- Pass 5 is not present at this revision. `wall_ms` is informational; retention compares certified quality, fingerprints, stop reasons, and proposal traces.

Hierarchy command:

```powershell
$env:REDA_EXTRA_CIRCUITS = 'ripple_adder8,alu4_full,multiplier4,alu8'
$env:REDA_RETENTION_BUDGET = [string][uint64]::MaxValue
cargo test --release --lib every_hierarchical_circuit_certifies_through_module_floorplan -- --ignored --nocapture
```

## Hierarchical baseline

| Circuit | Budget | Settle | Blocks | Volume | Static delay | Evaluations | Stop | Candidate fingerprint | Wall ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| ripple_adder8 | 0 | 608 | 70603 | 1123332 | 678 | 0 | EvaluationBudget | `a5e71ef0712baf6239bedd6781a75277c8d3b40170046750b01e1e3fdb8fb1b2` | 9802 |
| ripple_adder8 | u64::MAX | 560 | 71133 | 1401988 | 670 | 54 | ProposalStreamExhausted | `479e1679256d41b3c2b6fbc7f1ef075153dd1ceb686ee337d2dd40d3bc80c743` | 221077 |
| alu4_full | 0 | 925 | 191062 | 3332000 | 940 | 0 | EvaluationBudget | `0060a3dc718981b35509a100b65f88dce3eff779c7423fefe0d56878781b5afc` | 34693 |
| alu4_full | u64::MAX | 904 | 191966 | 3767200 | 924 | 78 | ProposalStreamExhausted | `9d43b24308bbeb07cf27cd75cdca82a81bd1a3079d3134eaae762e4adcd8e05a` | 474793 |
| multiplier4 | 0 | 1039 | 124948 | 3017412 | 1070 | 0 | EvaluationBudget | `5cc4f911455c7e5d4a1057e4123bc74c2ffe0c4229203bf7502b07ceee62732b` | 72455 |
| multiplier4 | u64::MAX | 1035 | 124660 | 3017412 | 1060 | 58 | ProposalStreamExhausted | `b2fdbba439a1cdc594cf9733aba98f0eafc388f9fa19bdf0b11965b1858a7038` | 2517982 |
| alu8 | 0 | 972 | 213833 | 2902664 | 1204 | 0 | EvaluationBudget | `19ea67df99e616c8b8789fc8e1119868c0ae588c2a8653d798bfa9b2e6f054ee` | 37780 |
| alu8 | u64::MAX | 970 | 213821 | 2902664 | 1200 | 34 | ProposalStreamExhausted | `bafea79d119cae16ebd6bbda7bdd3de7e70f18f18bf1c6ca32aac0989da9e2ed` | 390512 |

Case fingerprints are stable between budget 0 and exhaustion: ripple_adder8 `b9ab139aa9726703df3cd0b9f7ed30d50c6a8e0c8b1e2bb4156024a179844573`, alu4_full `a61d69e6372bee8a9fc1ff328dd84bec4b75e198f1dc8f4c445da3e59ce54e03`, multiplier4 `4a0a25fc8beb26ac272b67ca139f7a6dffa8af0b755256bfb71f2c326db82048`, and alu8 `856636a25601cb533299b0daf1b8e43034d6959a8593e4af91518153a1235c02`.

## Pinned child hierarchy

```powershell
cargo test --test build_circuit_pins hierarchy_with_a_child_preserves_requested_pins_through_exhaustion -- --exact --nocapture
```

Result: PASS at budget 0 and `u64::MAX`. The direct topology-aware seed fixture pins input `a` at `(50,1,20)` East and output `y` at `(200,1,20)` East and checks exact handover/net cells.

## Flat control

Baseline capture command:

```powershell
cargo run --release --bin fragment_baseline -- --output $env:TEMP\reda-refresh-relocation-flat-control-baseline.json --replace
```

Pre-feature acceptance command:

```powershell
cargo run --release --bin fragment_acceptance -- `
    --baseline $env:TEMP\reda-refresh-relocation-flat-control-baseline.json `
    --output $env:TEMP\reda-refresh-relocation-before-369dbbc5-0495-4857-b730-8db6d2a9cd9d\acceptance.json `
    --shipping-source $env:TEMP\reda-refresh-relocation-before-369dbbc5-0495-4857-b730-8db6d2a9cd9d\shipping_config.rs `
    --shuffle-seed 0x5245444120260831
```

- Exit: 0
- Verdict: `replacement_gate_passed=false shipping_evaluations=None failures=30`
- Acceptance JSON: `C:\Users\LTY\AppData\Local\Temp\reda-refresh-relocation-before-369dbbc5-0495-4857-b730-8db6d2a9cd9d\acceptance.json`
- Acceptance JSON SHA-256: `90514909F8886A247A1E40C59D92ADB46E1256BCA7AB6EB77D299909AC5BBDD2`
- Shipping source: intentionally absent because the replacement gate is false.

All 30 failures are quality-gate misses across the six cases and five budgets; every measured case compiled and certified. This is an existing flat-path replacement miss, not a crash or a Pass 5 result. Task 5 must reuse the exact baseline file/hash and reproduce this control without introducing a new flat-path failure.

Post-feature acceptance command used the same baseline and shuffle seed, writing to `C:\Users\LTY\AppData\Local\Temp\reda-refresh-relocation-after-07e32f00-9eb4-4614-aa40-a7194166a759`.

- Exit: 0
- Verdict: `replacement_gate_passed=false shipping_evaluations=None failures=30`
- Acceptance JSON SHA-256: `90514909F8886A247A1E40C59D92ADB46E1256BCA7AB6EB77D299909AC5BBDD2`
- Shipping source: absent, as in the pre-feature control.

The post-feature acceptance JSON is byte-for-byte identical to the pre-feature JSON. Pass 5 therefore introduces no flat-path change or new failure.

## Post-feature retention

Provenance:

- Exact structural solver commit: `a2bc712d1105e5a0869b29bb96524ea6d2e191fd`
- Exhaustive real/worker gate commit: `cff3516b2eec64bfd66d71a34d02a9acd4aeb32a`
- Hierarchy transcript: `C:\Users\LTY\AppData\Local\Temp\reda-refresh-relocation-post-feature.txt`
- Hierarchy transcript SHA-256: `58BE2961F475AD562A33823C2EE7399F0BEA20D57366D04A11AAD26780CD0E32`
- Harness exit/result: 0; 1 passed, 0 failed; 6156.74 s total.

The exact implementation exhausts Pass 5 after the unchanged Passes 1-4. All budget-zero rows and all four case fingerprints match the pre-feature transcript.

| Circuit | Settle before → after | Blocks before → after | Volume before → after | Static before → after | Evaluations before → after | Accepted Pass-5 indices | Post candidate fingerprint | Post wall ms |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| ripple_adder8 | 560 → 550 | 71133 → 71133 | 1401988 → 1401988 | 670 → 670 | 54 → 67 | 57, 60 | `f1e6b3025d27cbccc72b4c5dfa124f16fcd750a7939295e4350736deba835e78` | 307570 |
| alu4_full | 904 → 844 | 191966 → 191966 | 3767200 → 3767200 | 924 → 924 | 78 → 104 | 78, 82, 88 | `1515e091678654ffc01db34cee683d9e8d1826efdf25ec59c10c3a90b36e1a80` | 843501 |
| multiplier4 | 1035 → 1033 | 124660 → 124660 | 3017412 → 3017412 | 1060 → 1060 | 58 → 81 | 61 | `fbb0e204b8516fb75de5154e10166b4875558cde635658c79080d0a40509ac3d` | 4096505 |
| alu8 | 970 → 970 | 213821 → 213821 | 2902664 → 2902664 | 1200 → 1200 | 34 → 47 | none | `bafea79d119cae16ebd6bbda7bdd3de7e70f18f18bf1c6ca32aac0989da9e2ed` | 739971 |

Every post-feature exhaustion stopped at `ProposalStreamExhausted`. Every retained Pass-5 entry completed the unchanged whole-world certification sweep, strictly reduced observed settle against its immediate incumbent, preserved blocks and volume, and did not increase static delay. Commit `a2bc712` replaces the rejected output-changing work cap with exact structural filtering; commit `cff3516` exhausts the real Pass-5 stream and checks every accepted entry.

Worker determinism: PASS. Full `u64::MAX` runs at worker budgets 1, 2, and 4 produced the same complete trace, 67 evaluations, `ProposalStreamExhausted`, final quality `(550, 71133, 1401988, 670)`, and candidate fingerprint `f1e6b3025d27cbccc72b4c5dfa124f16fcd750a7939295e4350736deba835e78`. The first retained Pass-5 proposal was index 57 at settle 552; the final retained result settled at 550. The combined worker gate finished in 1243.07 s.

Post-feature pinned IO: PASS. `hierarchy_with_a_child_preserves_requested_pins_through_exhaustion` passed at budget zero and exhaustion in 3.80 s with the exact requested input/output coordinates and handover/net cells unchanged.

Retention decision: **KEEP Pass 5**. Three non-synthetic acceptance circuits retain fully certified lower-settle Pass-5 proposals with blocks and volume unchanged and static delay non-increased; worker determinism, pinned IO, hierarchical retention, and flat control all pass.
