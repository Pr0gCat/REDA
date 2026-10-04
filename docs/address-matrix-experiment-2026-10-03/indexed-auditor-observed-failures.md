# Retained auditor checks

The initial indexed-auditor run reported an ideal-state AssertionError in test_audit_shared_matrix_premise.py; a concurrent ROM32 audit reported JSONDecodeError, and its retry reported an ideal-state AssertionError. These outputs are not acceptance evidence. The wave1 bytes match the separately retained gzip exactly, and every JSON record and direct ideal-site comparison subsequently passed. The initial cause remains unexplained.

A frozen-source confirmation of the complete ROM32 audit passes in 4.959 seconds. Three further complete historical-ROM16x16 plus ROM32 replays produce exactly the pre-indexing audit JSON (7.519, 7.519, 7.570 seconds per pair). Existing shared-premise test retry passes. No verifier, quality assertion or immutable record was changed. Indexed changes remain uncommitted pending final check; the earlier committed width-derived auditor remains available.
