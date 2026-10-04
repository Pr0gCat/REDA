# Actual native build_circuit entry and saved-world verification

A frozen clean legacy/inout producer runs both native and auto modes with nonexistent Python. Both commands succeed and report native. Both exported block files and pinout JSON are exactly equal. Their Litematic files independently reload to the reference compiled-world fingerprint.

Each saved world is replayed for all 512 ordered setup/target phases continuously, checking eight public lamps against independent XOR-5/XOR-10 truth. No world reset or increased settling cap is used. The earlier clean inout qualification separately checks every implemented gate in the identical circuit world. This establishes the actual Litematic-producing native entry for this hierarchy, not complete native-feature coverage or real-game conformance.

Source, executable/library hashes, commands, raw artifacts, the reload verifier and terminal outputs are retained. An initial verifier incorrectly toggled the lever power field instead of its lit field; its negative output is preserved. The corrected verifier uses the existing simulator lever API and changes no simulator rules. An initial CLI run used a shared build target whose executable later changed during a separate compilation; it was not used for source-bound qualification. The qualified commands rerun against a copied immutable producer and verify its hash before and after execution.

The producer source base is committed 92bf87e plus the independently qualified legacy process patch (equivalent to 1b8eed4). This run predates the public-output occurrence core correction; that separate correction and its CLI integration remain separately qualified. No binary or library availability is implied after the recorded temporary files are removed.
