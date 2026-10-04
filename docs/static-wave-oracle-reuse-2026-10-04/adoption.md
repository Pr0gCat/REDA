# Reuse fixed Boolean oracle values during waveform auditing

The cover waveform auditor reevaluated fixed source and implementation graphs at every phase. The full protocol already proves that every phase address is in the enumerated state space, and these graphs are combinational and fixed within each record. Evaluate each graph once per address, then reuse its immutable Boolean values while retaining every per-phase state, event replay, landing, source/implementation, quality and protocol check. No simulation or verifier rule changes.

For one six-input record, graph evaluation calls fall from 16385 to 128. Extra storage is two address-indexed Boolean-value tables; the existing complete protocol and recorded phase histories already scale with the square of the number of states. This is an auditor implementation optimization, not a new synthesis method or production six-input admission.

The unchanged cover auditor suite passes three tests, including internal-event and landing late/unknown/deleted/repeated events, incorrect bindings and incomplete coverage. The unchanged balanced-matrix suite passes one test, covering multiple retained families and six corruption controls. No tests, ignores, controls or assertions change.

The complete ROM64 8192-phase audit is rerun against the same frozen original and matrix logs. Its entire result object equals the previously committed independent result, including all quality components and every regression list. Current measured execution time is recorded in complete-rom64-result.json. There is no controlled before/after wall-time ratio claim. Previous original tool sources remain preserved under rom64-complete-control-2026-10-04.
