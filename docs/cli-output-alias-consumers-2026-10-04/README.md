# Actual CLI output occurrence acceptance

Baseline source is clean commit `a3b47a4`. The final metadata producer is the same source plus `final-output-label-owned.patch`, changing only `src/bin/build_circuit.rs`. Both `native` and `auto` run the real build_circuit and mc_dump binaries with PATH set to `/nonexistent-python-path`. No Python/Yosys backend is available to these calls.

The baseline harness runs four build/dump pairs. It imports the generated pinout with a duplicate-key-rejecting Rust JSON visitor, checks public stdout and source-ordered OUTPUTPORT records, reloads the actual emitted Litematic, compares every loaded BlockState to the source compilation, then runs every retained setup/target truth phase. The two-input rewrite fixture has distinct sealed alias lamps; the six-input raw fixture has one shared alias lamp. Native and auto together cover 32 + 32 + 8192 + 8192 = **16,448 continuous phases**, all passing at the unchanged 2000 settling cap. This tests settled truth, not a new QoR claim.

The final metadata harness runs six new build/dump pairs: both modes for rewrite, raw and the collision fixture. It requires all aliased-interface outputs to use public keys, a complete duplicate-free output_labels mapping, complete stdout, valid source-ordered dump records and existing implementation bindings. Every new reloaded world and every physical/INPUT/GATEOUT/GATE dump record equals the frozen baseline. The collision only renames public labels: y[0] -> g1, y[1] -> other, z -> alias. Its implementation bindings and physical cells exactly equal the already tested rewrite. Therefore the settled-truth evidence is reused without replaying the 16,448 phases.

The compatibility maps in baseline/ retain the old mixed-key schema solely as the frozen physical/truth control. **Final consumer schema acceptance is in final-metadata/** and final-metadata-report.json; it does not rely on old mixed-key acceptance.

The Rust harnesses and exact sources are included. No existing world goldens or production source were changed by this acceptance work. manifest.sha256 binds every artifact; source-bindings.sha256 binds both binary builds and the changed producer source. Original sources are also bound by each report's SHA-256.

## Root read-only acceptance command

The final metadata-only audit reruns six actual binaries against the frozen physics. It does not rerun simulation:

```sh
source /home/seith/.cargo/env
ulimit -s 524288
CARGO_INCREMENTAL=0 RUST_MIN_STACK=536870912 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/reda-cli-label-target-a3b47a4 cargo test --manifest-path /tmp/reda-cli-label-clean-a3b47a4/Cargo.toml --test cli_alias_metadata -- --nocapture
```

It reads baseline files from /tmp/reda-cli-alias-consumers-20261004 and writes new metadata artifacts only under that directory. Final binaries are built from /tmp/reda-cli-label-clean-a3b47a4. Source hashes and patch are included for auditing these scratch paths.

## Reproduction from frozen source

Archive clean a3b47a4 src, Cargo.toml, Cargo.lock and tests/fixtures into /tmp/reda-cli-alias-clean-a3b47a4. Copy harness-baseline.rs to its tests/cli_alias_consumers.rs, then run:

```sh
source /home/seith/.cargo/env
ulimit -s 524288
CARGO_INCREMENTAL=0 RUST_MIN_STACK=536870912 CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/reda-cli-alias-target-a3b47a4 REDA_CONSUMER_ARTIFACT_DIR=/tmp/reda-cli-alias-consumers-20261004 cargo test --manifest-path /tmp/reda-cli-alias-clean-a3b47a4/Cargo.toml --test cli_alias_consumers -- --nocapture
```

Archive the same base into /tmp/reda-cli-label-clean-a3b47a4, apply final-output-label-owned.patch with patch -p1, and copy harness-final-metadata.rs to tests/cli_alias_metadata.rs. Run the metadata-only audit command above. Do not overwrite an existing frozen baseline when reviewing it; use a separate copy for full reproduction.
