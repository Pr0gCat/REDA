#!/usr/bin/env bash
# Everything that must be true before a change is considered done.
#
# The root crate's tests and clippy pass without ever building `viewer/`, which
# is a separate crate depending on `reda`. Adding a variant to a public enum in
# `src/compile/topology.rs` has twice left the viewer unbuildable while the root
# crate reported clean -- cd186ad (SecondTorch) and a0b485c (IsolatingRepeater).
# Checking both is the only thing that catches it.
set -euo pipefail
export PATH="/c/Users/LTY/.cargo/bin:$PATH"
cd "$(dirname "$0")"

echo "== root: test =="
cargo test --release 2>&1 | grep -E "^test result" |
  awk '{s+=$4; f+=$6; i+=$8} END {print "passed="s, "failed="f, "ignored="i; if (f>0) exit 1}'

echo "== root: clippy =="
cargo clippy --all-targets -- -D warnings 2>&1 | tail -1

echo "== viewer: test =="
(cd viewer && cargo test --release 2>&1 | grep -E "^test result" |
  awk '{s+=$4; f+=$6} END {print "passed="s, "failed="f; if (f>0) exit 1}')

echo "== viewer: clippy =="
(cd viewer && cargo clippy --all-targets -- -D warnings 2>&1 | tail -1)

echo "== viewer: wasm builds =="
(cd viewer && cargo build --release --target wasm32-unknown-unknown 2>&1 | tail -1)

echo "OK"
