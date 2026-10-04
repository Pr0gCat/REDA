# Disjoint-support composition — rejected experiment

The six-input, nine-declared-output source passed all 8192 retained from/to phases for both the original default compiler and the experimental composition. Public aliases are retained independently; canonical raw output alias metadata was repaired separately in the production compiler.

The candidate was rejected by whole-default physical waveform comparison. The first 0→1 target phase regressed settle ticks from 3 to 11, queued events from 2 to 5, and physical cell Boolean events from 34 to 43. Output `u` driver/lamp arrival regressed from 2/3 to 10/11. Smaller occupied-cell count was not an acceptance criterion. Full per-phase, per-channel observations and regression counts are in `whole-default-rejected/`.

Production proposal hooks, module export, and temporary visibility changes were withdrawn. No rejected generic composition is enabled or claimed complete. Exact experimental sources and pre-rollback shared-source snapshots are archived under `experimental-sources/`; established tests were not removed or ignored. Shared public-output occurrence metadata remains intact. `cargo check --lib` passed after rollback.

`whole-default-rejected/protocol-sources/` contains exact source bytes matching every SHA256 recorded at the experiment freeze. `protocol-source-snapshots.json` records their provenance and retained producer executable/library hashes; `producer-dependencies.d` records producer dependencies. Executable/library bytes remain in target rather than the evidence directory. Files outside the original eleven-source hash manifest are not retroactively claimed to have execution-time snapshots.

Next experiment must run in isolation, preserve fast direct/small cones, and require each replacement leaf to dominate its original control waveform before composition. The broader generic certified leaf-composition goal remains incomplete.
