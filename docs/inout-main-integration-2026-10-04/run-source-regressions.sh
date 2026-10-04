#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
run_check() {
    local proof_name="$1"
    shift
    set +e
    "$@" >"$proof_dir/$proof_name.log" 2>&1
    local proof_exit=$?
    set -e
    printf '%s\n' "$proof_exit" >"$proof_dir/$proof_name.exit"
    tail -n 8 "$proof_dir/$proof_name.log"
    return "$proof_exit"
}
run_check semantic cargo test --offline --lib frontend::resolved::tests -- --nocapture --test-threads=1
run_check source-regressions cargo test --offline --test systemverilog_compiler --test systemverilog_arithmetic --test systemverilog_parameters --test systemverilog_instances --test systemverilog_reset --test systemverilog_dff -- --nocapture --test-threads=1
