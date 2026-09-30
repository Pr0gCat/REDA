# Internal fanout isolation experiment (rejected)

Control: `26370a0`, with ordinary public-output isolation already enabled. A stock native full-family scan first fails at encoding 157, ROM words `[5,3,2,0]`, after encodings 0 through 156 pass all setup/target histories and deterministic recompilation. The previous failing table `[1,3,2,0]` now passes.

At address 0, internal `g3 = NOR(n3,n0)` should be true but its wall torch `(38,1,35)` is off. Its `n0` input repeater `(37,1,34)` is lit even though `n0 = a` is false. `n0` also has a bare dust landing into `n1 = NOR(n0,g0)`; the other `g0 = !b` input lands through a repeater and is true. This energizes the shared `n0` route backward and carries false power into `g3`. The route ledger is checked against the exact emitted world before attributing these cells. The parameterized manual diagnostic reproduces the incorrect internal driver:

```
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 5,3,2,0
```

Experiment: extend the existing pre-routing diode requirement to every net with more than one consumer (`consumers.len() > 1`), in addition to all public outputs. This uses connectivity rather than table/module identity.

The experiment passes all 4096 stock tables, deterministic recompilation, and 65536 target transitions plus 65536 setup phases. This is evidence for the physical mechanism, not permission to accept a quality regression.

Rejection evidence: the fixed quality matrix fails `xor2/native` mean settling at 7.875 ticks. The independently accepted cap is preserved. The implementation is withdrawn; no quality caps, physical checks, or observer durations are changed. The temporary full-family stock test is also removed rather than committed as a failing or ignored acceptance test. The manual diagnostic retains the new table and real neighborhood evidence for continued investigation.

This demonstrates that protecting public outputs alone does not qualify arbitrary internal fanout. Automatic stock/torch-row selection remains disabled. A future solution must prevent internal backward power without violating accepted timing and glitch controls; merely adding diodes everywhere is insufficient.
