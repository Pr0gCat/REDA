#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
cd "$proof_dir/index-candidate"
set +e
cargo test --offline --lib frontend::resolved::tests -- --nocapture --test-threads=1 >"$proof_dir/final-semantic.log" 2>&1
proof_exit=$?
printf '%s\n' "$proof_exit" >"$proof_dir/final-semantic.exit"
tail -n 10 "$proof_dir/final-semantic.log"
set -e
if (( proof_exit != 0 )); then exit "$proof_exit"; fi
set +e
bash "$proof_dir/run-index-lint.sh"
set -e
bash "$proof_dir/run-wasm.sh"
