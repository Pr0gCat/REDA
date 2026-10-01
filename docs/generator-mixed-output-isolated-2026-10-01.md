# Independent mixed-output comparator prototype

Reference `b5f264e`. Instrument the rejected upper layout without changing
simulator behavior: observe actual comparator state and strong/weak support
power, then cut individual dust cells before initial settlement and replay
each complete history. Coordinate cuts are causal diagnostics, not generation
selection rules. The diagnostic source and seven complete cut histories are
archived beside this report.

At address 1, the NOT-a comparator actually has output zero and its primary
side support has strong 15; nevertheless its public dust driver stays high.
Thus the comparator's Boolean operation is not the failing component. Cutting
upper dust `(9,2,5)` restores that driver's expected four-address pattern but
disconnects the NOT-b public branch. Exporting the internal handover output
through that upper route introduces an unwanted public conductor path. This
does not prove that this single cut repairs every layout or certify a complete
electrical ownership graph.

The corrected independent prototype avoids exporting the handover support.
A second NOT-b comparator reads the opposite side of the same primary support
and feeds a private public output rail. NOT-a and the AND rear likewise share
their original primary support. Reference torches and public conductors are
separated; the AND output detours south to avoid the new negative output rail.
There are still exactly two independently driven input levers.

Both input permutations pass 32 retained-history phases, including every
setup and target transition. Compare the two functions against their immutable
certified candidate records with the existing twelve-metric quality helper:

| Metric | Frozen candidate | Independent prototype |
| --- | ---: | ---: |
| Worst settling | 10 | 8 |
| Total target settling | 114 | 68 |
| Driver last change | 6 | 4 |
| Lamp last change | 10 | 8 |
| All driver/lamp glitch and pulse metrics | 0 | 0 |

Both complete physical repeats are exact. These are local prototype results,
not a 4096-family qualification, source rewrite proof or physical certificate.
`certified:false` remains explicit. No default compiler selection changes.

Reproduce with `source tools/env.sh` and
`RUST_MIN_STACK=67108864 cargo run --release --example comparator_mixed_outputs_diagnostic`.
The optional `--cut=x,y,z` acts on this prototype; reproduce the original upper
cuts using the separately archived upper diagnostic source instead.

Next integrate shared primary-support fanout, separate selected comparator
instances and owned public rails into typed candidate construction. Recognition
must use condition structure and output dependency rather than these witness
encodings. Then run strict certification, source equivalence, full 4096 fixed
comparisons, setup metrics, prior caps, exact repeat, and appropriate regressions
before adopting the method or integrating automatic selection.
