# Keep compact routing reports tied to their actual compiler

The compact-channel repeater test called the public rewrite selector and assumed it returned a legacy channel layout. The qualified seven-segment selector now returns FragmentSynth, so the test failed before checking any compact-emitter property. It now uses the existing `compile_without_rewrite` independent control for those properties. Every original spacing, sharing, span-aware feedthrough, size comparison and exact repeater-count assertion remains unchanged.

The same test additionally builds the public selected result, verifies FragmentSynth, and requires both legacy-only report APIs to reject that layout explicitly. This preserves the original control and strengthens the consumer contract; it is not acceptance of circuit quality based on size or volume. All seven routing-statistics tests pass in 43.81 seconds. No production generation, simulator or verifier changes are made by this test repair.

Two sandbox builds failed before tests ran (SIGSEGV and LLVM allocation failure). The identical test command completes outside that sandbox. The failed `compile_legacy` experiment is also retained: that explicit noncompact emitter uses spacing 5, while the unchanged compact assertion requires spacing 3. The fitting owner is the existing independent compiler control, not a new policy or a relaxed expectation.

This repairs one observed root failure. Full-project convergence and the separately pending arithmetic/search qualification are not claimed complete.
