#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
set +e
cargo test --offline --no-fail-fast --test systemverilog_compiler --test systemverilog_parameters --test systemverilog_instances --test systemverilog_reset --test systemverilog_dff -- --nocapture --test-threads=1 >"$proof_dir/remaining-regressions.log" 2>&1
proof_exit=$?
printf '%s\n' "$proof_exit" >"$proof_dir/remaining-regressions.exit"
tail -n 15 "$proof_dir/remaining-regressions.log"
exit "$proof_exit"
