# Matrix waveform audit follows the source boundary

The offline auditors previously required four inputs, 16 addresses and 512 setup/target phases. They now derive address count from the unique source input list and require exactly two phases for every ordered address pair. Oracle bit, row and mask bounds follow the truth-table width. Implementation and source input widths must agree.

All graph evaluation, complete event replay, public truth, per-phase and per-channel quality comparisons remain required. Offline replay still reports release_qualified=false and does not grant a physical certificate. No historical control, simulator, verifier or wait limit changed.

Validation: python3 tools/test_audit_matrix_width.py covers complete 4/5/6-input protocols and rejects truncated phases, missing events, changed source truth and out-of-range oracle bits/rows. Existing test_audit_balanced_matrix_wave.py and test_audit_shared_matrix_premise.py pass. Replaying retained ROM16x16 default repeat1 against its unchanged control gives exactly the retained audit JSON.

The separate isolated five-input candidate passed strict Rust certification and 2,048 public-truth phases. Its first complete internal replay checks 1,081 implemented gates and 1,134 observation sites with zero hidden extra transitions. Three separately retained waves/worlds agree completely. Wider generation is not adopted: original-method quality comparison, full boundary coverage and prospective production checks remain outstanding.
