#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != --bounded ]]; then
 exec bash /home/seith/REDA/tools/run_memory_bounded.sh bash /home/seith/REDA/target/qualification/native-rom64-selection-diagnostic/run.sh --bounded
fi
cd /home/seith/REDA/target/qualification/native-rom64-selection-diagnostic/source
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=/home/seith/REDA/target/qualification/native-rom64-selection-diagnostic/build
set +e
cargo test --offline --locked --lib --no-run > ../build.log 2>&1
result=$?
printf '%s\n' "$result" > ../build.exit
if (( result != 0 )); then exit "$result"; fi
python3 ../bind-binaries.py
result=$?
if (( result != 0 )); then exit "$result"; fi
cargo test --offline --locked --lib compile::selection::rom64_selection_diagnostic::ordinary_physical_entry_stage_probe -- --exact --test-threads=1 --nocapture > ../probe.log 2>&1
result=$?
printf '%s\n' "$result" > ../probe.exit
exit "$result"
