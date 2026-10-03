# Matrix runtime certification context

Certified physical rewrites now retain the complete library used to expand their implementation. Library IDs are local to that roster; substituting the default library for the matrix library fails strict primitive verification. Cloning preserves all library maps, including stateful and comparator contracts.

`CertifiedPhysicalRewrite::certify_runtime` uses the retained candidate, implementation graph, library and exact emission extent. It runs the existing strict physical, equivalence, timing and bounded functional checks. Certification caps and simulator rules are unchanged.

Circuit evaluation validates the existing sealed rewrite view and checks the implementation input manifest. Logical statistics continue to describe the source. It neither invents positions for removed source gates nor renames measured manifests.

The native seven-segment test passes in 41.26 seconds: emitted world fingerprint matches the retained world; wrong default library, cropped extent, zero transition cap and zero tick cap are refused. Evaluation accepts the matching rewrite and rejects another source and tampered gate observations. Four existing evaluation tests pass. WASM library check passes with existing warnings. The initial exact-filter invocation ran zero tests and is not acceptance evidence.

This is a certification and consumer correction, not a new measured circuit optimization. Recursive root/leaf integration, pinned composition and the eight-item project goal remain incomplete. The original pre-clock ROM experiment remains live in session 17896 with no completed control record observed; it was not restarted.
