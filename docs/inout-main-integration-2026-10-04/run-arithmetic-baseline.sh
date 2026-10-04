#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/seith/REDA/target/qualification/main-inout-integration/baseline-target
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
cd "$proof_dir/baseline-root"
set +e
cargo test --offline --test systemverilog_arithmetic arithmetic_keeps_width_and_unsupported_operator_validation -- --exact --test-threads=1 >"$proof_dir/arithmetic-baseline.log" 2>&1
proof_exit=$?
printf '%s\n' "$proof_exit" >"$proof_dir/arithmetic-baseline.exit"
rg 'running [0-9]+ test|^test .* \.\.\.|^test result:|^error:|invalid arithmetic accepted' "$proof_dir/arithmetic-baseline.log" | cut -c 1-180
exit "$proof_exit"
