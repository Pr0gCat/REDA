# Guarded internal inverter bypass

Control: `32150bf`. Repeated inversion rails can leave a physical `NOT(NOT(x))` between a signal and its consumers even when it has no public output. Removing that path is a Boolean identity, but the earlier unconditional experiment regressed the already-fixed ROM `[1,3,2,0]`: at address 0 its `g3=!a` driver became false instead of true. The exported `!b` repeater into `n1` repowered the newly shared primary-input `a` dust and corrupted the sibling inverter. The exact rejected experiment is retained separately as `generator-double-inversion-unguarded-rejected-2026-09-30.patch`; it is not the accepted implementation.

The accepted optimization bypasses an internal second inverter only when every consumer of both the bypassed signal and the original signal is itself `Nor(1)`. It excludes shared multi-input supports, merges, and sequential consumers. Exported gate outputs remain real named gates; output order and aliases are preserved. If the first inverter becomes unconsumed and is not exported, it is removed too. Leaving that orphan was experimentally insufficient: a four-stage chain's total settling regressed despite the Boolean bypass.

Only optimized lowering applies the rule. Positive lowering remains unchanged and supplies the independent reference. The rule depends on signal connectivity and gate kinds, never module/case names. Gate/block counts and occupied volume are not quality criteria. Removed gates are filtered from physical provenance and source terminal indices are remapped; optimized-away internal cells have no fabricated observation site.

## Physical results

The independent reference and candidate are compiled and simulated for nine chain lengths (2 through 10) and 16 two-output inverter fanouts (four-stage common prefix and branch lengths 1 through 4). Every setup and target phase checks every real internal driver and independently expected output driver/lamp truth. Both arms retain physical history and use the same settling limit. Repeated compilation must have identical world fingerprints.

All 25 cases preserve all nine individual timing/glitch/pulse metrics. Current reference and candidate must also stay within the separately frozen reference metrics in `generator-inverter-bypass-2026-09-30.json`; later reference degradation cannot raise a cap. Each chain of length 3 through 10 and every fanout case has strictly lower worst output-driver change time. The two-stage exported buffer remains unchanged.

| Original chain length | Reference driver worst ticks | Candidate driver worst ticks | Reference world worst ticks | Candidate world worst ticks |
| --- | ---: | ---: | ---: | ---: |
| 2 | 4 | 4 | 8 | 8 |
| 3 | 6 | 2 | 10 | 6 |
| 4 | 8 | 4 | 12 | 8 |
| 6 | 16 | 4 | 20 | 8 |
| 10 | 32 | 4 | 36 | 8 |

The fixture comparison covers 100 target transitions and 100 setup phases per arm (400 physical phases total), plus initial settling. Complete traces, world fingerprints, and nine metrics are frozen with the independent reference. Exported intermediate outputs and duplicate output occurrences have an additional logical boundary test. Both the original-signal and bypassed-signal multi-input fanout guards are tested.

## Regression and limits

- The production frontend and generator quality suites pass all existing controls across 1312 measured transitions, including native and fresh-Yosys cases.
- The observed-output isolation regression retains all physical histories and renamed/reordered variants.
- Four constant-driver tests and the native/fresh-Yosys physical DFF test pass.
- All 19 lowering unit tests pass, including provenance and library-error behavior.
- All three new inverter-bypass tests pass against current and independently frozen controls.

No accepted assertion or physical observation interval is weakened. This does not claim the complete repository suite is green, universal timing improvement, or Minecraft certification. The earlier internal/multi-input backfeed defects remain outside this guarded optimization; automatic torch-row selection remains disabled. The generator optimization goal remains active.

Reproduce:

```
source tools/env.sh
RUST_MIN_STACK=33554432 cargo test --release --test inverter_bypass_quality
RUST_MIN_STACK=33554432 cargo test --release --lib compile::lowering::tests
```
