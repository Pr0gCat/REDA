#!/usr/bin/env bash
set -euo pipefail
cd /home/seith/REDA
if [[ ${1:-} != --bounded ]]; then
 exec bash tools/run_memory_bounded.sh bash target/qualification/native-inout-selected-repair/run.sh --bounded
fi
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=/home/seith/REDA/target/qualification/native-inout-selected-repair/build
export REDA_PYTHON=/home/seith/REDA/.venv/bin/python
set +e
cargo test --offline --locked --lib frontend::resolved::tests -- --test-threads=1 > target/qualification/native-inout-selected-repair/semantic.log 2>&1
result=$?
printf '%s\n' "$result" > target/qualification/native-inout-selected-repair/semantic.exit
if (( result != 0 )); then exit "$result"; fi
cargo test --offline --locked --test native_inout_selected_assignments --test native_physical_inout -- --test-threads=1 > target/qualification/native-inout-selected-repair/physical.log 2>&1
result=$?
printf '%s\n' "$result" > target/qualification/native-inout-selected-repair/physical.exit
exit "$result"
