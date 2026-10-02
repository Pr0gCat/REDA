# Validate the selected primitive graph

Both segment reference partition callers passed the ideal library expansion to the default compiled world. Qualified defaults carry a certified physical rewrite, so the unchanged partition verifier correctly refused them with `CertifiedRewriteViewMismatch: graph differs from certified implementation`. This was a caller contract error rather than unexplained world geometry.

The reference helper now follows the existing equivalence helper: independently compile without rewriting and check that original world against its original library expansion; then compile the default and check it against the graph belonging to its certified rewrite, or the original expansion when no rewrite exists. Both worlds retain the complete partition assertion that no non-air block is unexplained. The shared assertion helper avoids duplicating that contract.

This repair changes only reference-check callers. Production generation, partition classification, physical certificate validation, simulator, frozen worlds, and quality thresholds are unchanged. The missing-input negative test remains required; six physical rewrite view tests retain wrong graph and tampered artifact rejection coverage. The reference partition suite passes all five cases, including complete seven-segment and segment_a coverage of both original and selected worlds.

The independent root comparison and test logs are stored with the matching JSON summary. Remaining root failures are still failures; the full eight-part goal remains active. The immediately preceding refresh-reserve change independently preserved three complete runs of the 4096 family and each large default waveform; those artifacts remain the generation controls for this caller-only change.

Final root result:1164 passed,17 failed,71 ignored (exit101), compared with1162/19/71 before this caller repair. Only the two partition reference failures are repaired; no new failures appear. The original clean goal-turn parent had20 failures, so the two qualified commits reduce that total by three. This remains a failing root suite.
