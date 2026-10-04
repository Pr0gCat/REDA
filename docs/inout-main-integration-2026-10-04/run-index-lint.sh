#!/usr/bin/env bash
set -euo pipefail
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
proof_dir=/home/seith/REDA/target/qualification/main-inout-integration
cd "$proof_dir/index-candidate"
set +e
cargo clippy --offline --lib --bin build_circuit --test native_physical_inout --message-format=json -- -D warnings >"$proof_dir/index-lint.jsonl" 2>"$proof_dir/index-lint.log"
proof_exit=$?
printf '%s\n' "$proof_exit" >"$proof_dir/index-lint.exit"
tail -n 8 "$proof_dir/index-lint.log"
exit "$proof_exit"
