# Merge bypass clearance supply repair

The native ROM's legacy route reached gate 27's output pin at (194, 1, 19) with strength 1. The mandatory clearance step to (194, 1, 18) reduced that signal to zero before the next refresh. The unchanged debug assertion then rejected the route: `a run cannot start from an already-dead signal`. The earlier negative diagnostic is retained with the matrix recursive-root evidence.

The existing bare-merge supply reserve now includes the actual unrefreshable clearance selected by `bypass_source_start`. The same reserve feeds the pure source-strength model and both physical bare-branch emission paths, including recursive bare-merge chains. Ordinary routes retain their existing reserve. There is no circuit-name exception, simulator change, relaxed cap, assertion removal, or altered verification policy.

## Independent bounded evidence

The final isolated SDK matches the working compiler source exactly. Its library and probes use `-C debug-assertions=yes`. Build arguments, source archives, binary hashes, raw records, worlds, and independent audit results are bound in the adjacent JSON manifest and artifact directory.

Three fresh repeats of all 4,096 historical family functions produce byte-identical immutable records: 12,288 worlds and 393,216 continuous phases. The historical 27 improvements and 4,069 holds remain unchanged. Six additional actual-default seven-segment and ROM worlds cover 3,072 phases. All phase records, intermediate and primitive events match the frozen actual-default records, except process-specific artifact paths. Public-channel arrivals and settlements have no regressions. This is a supply correctness repair, not an additional measured quality improvement.

The standalone audit deliberately reports `certified: false` and `release_qualified: false`; its replay result alone is not project or Minecraft acceptance. The source-bound executable performs the existing runtime certification. The archived earlier default probe succeeds with debug assertions enabled for both circuits.

## Remaining acceptance

Clippy and the WASM compile check exit successfully with existing warnings. The original unoptimized Cargo default integration (session 93604) and complete library run (session 56809) remain live at this snapshot; after more than 43 minutes they have not returned a terminal result. Nine library failure names have appeared, all present in the immutable historical failure log. Equal names do not establish equal causes. The runs are neither terminated nor counted as successful.

The original pre-clock search driver is separate and does not validate this repair. The full eight-item completion goal remains active. General matrix composition, sequential quality, HDL gaps, Yosys-absence acceptance, real Minecraft conformance, and complete project checks remain unfinished. Existing user changes, including the family probe and sequential growth fallback, are excluded from this repair commit.
