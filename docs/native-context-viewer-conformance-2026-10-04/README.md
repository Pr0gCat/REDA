# Native signed contexts: viewer compiler portability

All three unchanged historical frontend_compiler controls pass native debug and Node/WASM release: four original netlist/debug fingerprints, original compact-channel world fingerprint, and separate default-seven-segment512-phase continuous-event qualification. No historical hashes were repinned.

New integral_contexts fixture checks all16 input words for signed extension, signed inversion, and mixed unsigned inversion, all8 public output bits, all origin ranges, JSON sidecar reload and repeated compilation determinism. Three new debug fingerprints are frozen from the independently checked native fixture; final native and WASM must match these same values. These are new fixtures, not replacements for historical controls.

wasm-pack first builds successfully but cannot locate its runner in no-install mode. That setup failure is retained. The installed matching wasm-bindgen-test-runner0.2.126 is then used explicitly through cargo; no installer or network was needed. REDA_PYTHON points to a nonexistent executable during WASM qualification.

Runs use the current working tree with unstaged arithmetic/backend changes. This establishes consumer portability for the declared fixture scope, not isolated whole-project release convergence, arbitrary browser source ingestion, or complete gap5/6/8.
