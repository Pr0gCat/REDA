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
    tail -n 10 "$proof_dir/$proof_name.log"
    return "$proof_exit"
}
cd "$proof_dir/index-candidate/viewer"
run_check wasm-build wasm-pack build --target web --dev -- --offline
run_check wasm-consumer node "$proof_dir/wasm-consumer.mjs"
python3 - "$proof_dir/index-candidate/viewer/index.html" "$proof_dir/viewer-module.mjs" <<'PYMODULE'
from pathlib import Path
import re, sys
modules = re.findall(r'<script\s+type="module"[^>]*>(.*?)</script>', Path(sys.argv[1]).read_text(), re.S)
assert len(modules) == 1
Path(sys.argv[2]).write_text(modules[0])
PYMODULE
run_check page-syntax node --check "$proof_dir/viewer-module.mjs"
