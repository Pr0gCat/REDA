# Multiple-support fanout experiment (rejected)

Control: `c862740`. The previous all-fanout diode experiment fixed stock ROM truth but violated XOR timing. This experiment instead added a pre-routing diode requirement when a net has more than one non-merge, multi-input gate consumer. Public-output isolation was unchanged.

The original `[5,3,2,0]` defect passes. All 1312 production quality transitions pass the fixed controls. Physical constant-driver tests (four), native/fresh-Yosys DFF test, and the three observed-output metamorphic variants pass. All nine metrics of the six ROM shipping comparisons are exactly unchanged against the independently frozen control.

Nevertheless, the deterministic full-family scan fails at encoding 1036, words `[4,1,0,2]`, after encodings 0 through 1035 pass. At transition 0 to 2, output bit 2 driver `(47,1,20)` is true instead of false. The generalized manual diagnostic identifies the first wrong internal driver `n1`, at `(40,1,22)`, which is false instead of true.

The graph has `g0=!b`, `n0=!a`, `n1=!g0`, `g2=NOR(n0,g0)`, and `g4=NOR(n1,n2)`. The `g0` fanout includes one inverter (`n1`) and one multi-input NOR (`g2`), so the new predicate omits it. The protected `n0` repeater into `g2` strongly powers its support and its bare `g0` dust landing backward. The `g0` branch into `n1` carries that wrong high signal despite `g0` itself being correctly low. A repeater at the receiving inverter passes the already-backfed signal; it does not isolate the origin of that backward power. Gate neighborhood evidence confirms this receiving repeater is lit. Exact route-ledger/world fingerprint agreement is checked before diagnostics.

Reproduction during the experiment:

```
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example rom_output_history_diagnostic -- 4,1,0,2
```

The experiment is withdrawn rather than accepted on the smaller passing matrix. The temporary full-family stock test is removed; none of the accepted tests or metric caps is weakened or ignored. All production code is restored exactly to `c862740`. No automatic torch-row selection is enabled.

The next hypothesis must account for reverse power escaping through inverter consumers as well as other multi-input supports. The quality-passing restricted rule cannot establish that safety property. Blanket diodes remain rejected by the prior timing evidence.
