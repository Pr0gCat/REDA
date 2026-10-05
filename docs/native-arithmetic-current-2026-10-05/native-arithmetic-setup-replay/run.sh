#!/usr/bin/env bash
set -euo pipefail
cd /home/seith/REDA
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648
base=target/qualification/native-arithmetic-setup-replay
library=target/qualification/native-output-contract-current/build/debug/deps
export CARGO_MANIFEST_DIR="$PWD/target/qualification/native-arithmetic-current/source"
rustc --edition=2021 -C opt-level=2 -C debug-assertions=yes -C overflow-checks=yes "$base/replay.rs" -L dependency="$library" --extern reda="$library/libreda-4f8f0b1f1134441b.rlib" --extern serde_json="$(find "$library" -name 'libserde_json-*.rlib' -print -quit)" -o "$base/replay" > "$base/build.log" 2>&1
artifacts=target/qualification/native-arithmetic-current/source/target/qualification/native-arithmetic-generation/arithmetic_counter_add
set +e
"$base/replay" "$artifacts" --no-setup > "$base/no-setup.log" 2>&1
result=$?
printf '%s\n' "$result" > "$base/no-setup.exit"
if ((result!=101)); then exit 76; fi
"$base/replay" "$artifacts" > "$base/setup.log" 2>&1
result=$?
printf '%s\n' "$result" > "$base/setup.exit"
exit "$result"
