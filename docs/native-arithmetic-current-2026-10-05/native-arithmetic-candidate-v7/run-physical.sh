#!/usr/bin/env bash
set -euo pipefail
cd /home/seith/REDA/target/qualification/native-arithmetic-current/source
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_PROFILE_TEST_OPT_LEVEL=2 CARGO_PROFILE_TEST_DEBUG_ASSERTIONS=true CARGO_PROFILE_TEST_OVERFLOW_CHECKS=true CARGO_PROFILE_TEST_DEBUG=0
export CARGO_PROFILE_DEV_OPT_LEVEL=2 CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=true CARGO_PROFILE_DEV_OVERFLOW_CHECKS=true CARGO_PROFILE_DEV_DEBUG=0
export CARGO_TARGET_DIR=/home/seith/REDA/target/qualification/native-output-contract-current/build
if pgrep -x java >/dev/null; then exit 75; fi
set +e
cargo test --offline --locked --test native_arithmetic_generation --no-run > ../../native-arithmetic-candidate-v7/build.log 2>&1
result=$?
printf '%s\n' "$result" > ../../native-arithmetic-candidate-v7/build.exit
if ((result!=0)); then exit "$result"; fi
for case in native_decrement_generates_correct_loaded_feedback; do
 cargo test --offline --locked --test native_arithmetic_generation "$case" -- --exact --test-threads=1 --nocapture > "../../native-arithmetic-candidate-v7/$case.log" 2>&1
 result=$?
 printf '%s\n' "$result" > "../../native-arithmetic-candidate-v7/$case.exit"
 if ((result!=0)); then exit "$result"; fi
done
