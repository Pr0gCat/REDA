# Native assignment contexts: owned-helper v3

Isolated prototype based on 58b4366; not production adoption. Context conversion now moves the owned RTL kind instead of cloning the full tree before dispatch; unsigned extension moves the source tree. Signed extension retains explicit sign-bit slices. The unused BinaryOp spelling helper is removed only in this clean prototype: shared user arithmetic still uses it and must be preserved during integration.

Clean target qualification exits 0: five diagnostic tests including 512 continuous physical phases, three supplemental provenance/truth/boundary tests, 25 compiler tests and eight hierarchy tests. All six formerly unsupported refusal sources are retained in exhaustive successful-source tests; source-migration-audit.json checks their verbatim preservation. All remaining negative controls pass.

The first reused-target run failed at linking with undefined symbols; first-link-failure.log is retained. A new independent target built offline and completed the same checks. This does not establish the cause of the initial linker failure.

The source manifest precedes the clean-target retry. Source attribution is not fully qualified: width ranges and declaration ownership are checked, but complete debug viewer and consumer contracts remain pending. Native/auto CLI runs with empty PATH are in progress and are not part of this qualification claim. Shared user changes were not edited. Broad gaps remain open.
