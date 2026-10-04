#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
cd "$proof_dir/index-candidate"
set +e
cargo test --offline --test native_physical_inout ordinary_native_physical_entry_exposes_actual_decoder_implementation -- --exact --nocapture --test-threads=1 >"$proof_dir/index-decoder.log" 2>&1
proof_exit=$?
printf '%s\n' "$proof_exit" >"$proof_dir/index-decoder.exit"
tail -n 14 "$proof_dir/index-decoder.log"
set -e
if (( proof_exit != 0 )); then exit "$proof_exit"; fi
bash "$proof_dir/run-wasm.sh"
