# Source output occurrence metadata

Adds CompiledCircuit::output_occurrences() with source-ordered signal names and positions captured by actual raw emission and sealed rewrite ports. Compatibility output_positions remains available. No geometry, route, physics, truth or budget changes.

Isolated cargo check --lib and cargo test --lib --no-run pass. Two dedicated occurrence tests and six existing physical_rewrite_views tests pass. Raw unique outputs exercise all three raw compiler APIs in reverse name order; rewrite aliases preserve distinct sealed lamps, identical world and all 16 setup/target pairs at the existing 2000 settling cap.

Known independent defect discovered by an additional experiment: raw duplicated output names are refused by the pre-existing legacy adapter / boundary ownership model. This patch does not claim to repair raw shared-boundary aliases; an explicit ownership repair is being qualified separately. Failure logs remain in /tmp/reda-output-occurrence-owned-20261004/fixtures-final.log and fixtures-adapter.log.
