# Fragment search contract audit

Inspected at `d65bd521ce7de2b04aecff1a57acb6e8c4b14353`, with the existing working-tree foundation retained. This is a source-flow audit, not a circuit experiment or a completed search implementation.

## Confirmed behavior

`compile_fragment_synth` computes the recursive case identity and calls `compile_recursive_contract`. That function builds one recursive product, reports zero evaluations and an empty trace, and reports the requested budget kind as its stop reason. No production proposal is evaluated. The API documents this limitation explicitly.

The budgeted proposal loop in `search.rs` is test-only. Restoring that loop alone would not satisfy the quality contract: its acceptance rule compares the lexicographic pair of worst settlement and static routed delay. It can accept a candidate with regressions on other transitions. Block count and volume are already diagnostic fields, not selection criteria.

The recursive producer owns structural composition. Its unpinned branch already tries nested packing and lid fabric on the dense and standard leaf ladders, stopping at the first successful certified product. These are genuine existing construction alternatives, but currently form a feasibility fallback, not a budgeted quality search. Pin normalization, exact caller-owned port cells, leaf refusals and whole-root certification belong to this producer and must remain authoritative.

Root certification measures transitions from warmed-base copies. `RootCertificate` retains individual transition measurements, but `assemble_product` reduces them to worst settlement and worst-transition indices. `TransitionMeasurement` does not contain public channel event histories. These records cannot establish continuous-history glitch, pulse or per-channel arrival dominance.

The public default selector in `selection.rs` already has the stronger runtime qualification: setup/target metrics, glitch-transition counts, every phase settlement and every public channel arrival. That qualification must be reused or coherently extended; reconstructing it from `CandidateMetrics` would lose information. Recursive caller-pinned ports require preserving their external drive and observation contracts rather than assuming ordinary generated levers and lamps.

## Consumers

- `build_circuit.rs` explicitly requests zero evaluations; preserve its baseline behavior.
- `routing_cost_report.rs` explicitly requests zero evaluations for recursive attribution; preserve its measured baseline.
- `fragment_synth/benchmark.rs` forwards the requested evaluation budget and consumes returned world, metrics and trace; this is an existing experiment entry for real budget comparisons.
- `evaluation.rs` consumes certified synthesis metrics; adding quality search must not substitute unqualified or differently bound metrics.

## Implementation boundary

Expose existing structural alternatives through the responsible recursive producer, preserving its certified composition and pin rules. Evaluate alternatives in fixed structural order, count actual attempted proposals, retain refusals, and stop before starting a proposal when its budget is spent. Time budgets may finish a proposal already started. A zero budget still produces the existing baseline.

Qualify each constructed root against the incumbent using the complete continuous protocol before acceptance. Require matched phase/channel identities, no individual quality regression and a strict improvement. Do not change simulator or physical verification rules. Do not admit block/volume improvements, reset histories, loosen caps or replace historical controls.

Keep the prohibition on the legacy whole-circuit seed. The source-inspection test currently also prohibits any proposal loop; revise that assertion only alongside a working recursive search and runnable replacement checks. Its existing no-search budget tests describe today's behavior and are not evidence of completed search.

Required experiment: budgets zero, one and multiple proposals; deterministic trace prefixes; certified refusals counted; exact pins; an aggregate improvement concealing a phase/channel regression rejected; representative composed worlds independently replayed against frozen controls. No such new experiment was run in this audit. All eight completion-goal items remain incomplete.
