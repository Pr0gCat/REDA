# Index matrix waveform events once per phase

Both matrix auditors previously scanned the complete event list separately for every observed signal. Each phase now groups events by signal once, preserving each signal's original event order. All source truth, implementation bindings, event replay, hidden transitions, public outputs and historical quality assertions remain unchanged.

Final check: `PYTHONPATH=tools python3 -m unittest tools.test_audit_balanced_matrix_wave tools.test_audit_shared_matrix_premise tools.test_audit_matrix_width` passes all four tests in 8.049 seconds. Existing tests replay complete seven-segment/ROM and constant boundaries, reject corrupted records, and check complete 4/5/6-input protocols. The source hashes also exactly match the [three-repeat comparison](address-matrix-experiment-2026-10-03/indexed-auditor-repeat-proof.json), whose historical ROM16x16 and ROM32 audit JSON matches pre-indexing results.

[Earlier unexplained failures](address-matrix-experiment-2026-10-03/indexed-auditor-observed-failures.md) remain recorded; their cause was not established. The final tests and complete repeated comparisons pass without altering records or assertions. This change removes repeated event scans; it makes no measured end-to-end speedup claim. It does not change generation admission or certify a Minecraft result.
