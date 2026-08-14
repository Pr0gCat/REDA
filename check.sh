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

# The stanza above and the one below both run on the host or build for wasm;
# neither ever *executes* wasm. So without this line the one property Task 12
# exists to establish -- that the browser's circuit is the compiler's circuit --
# would be verified exactly once, by hand, and never again. A guarantee nobody
# re-runs is the same class of defect as a test that cannot fail, which this
# branch has now shipped four times.
#
# **The count, not the exit status.** Measured 2026-08-15 by deleting the
# `#[wasm_bindgen_test]` attribute: the runner prints `no tests to run!` and
# **exits 0**. `awk` therefore refuses `s<1` as well as `f>0`, and `pipefail`
# catches the case where no `test result` line is printed at all.
#
# Unfiltered, and not `-- --test placement_agrees_with_native`. Naming the one
# file reads better and costs 1.5s less, and it means the next wasm test
# somebody adds is silently outside the gate -- which is this stanza's own
# failure mode one level up. What the unfiltered run prints instead is five
# `no tests to run!` lines (the viewer's other test targets carry plain
# `#[test]`, which the wasm harness does not collect; their 21 native tests are
# the stanza above's job) and a `Doc-tests reda_viewer` summary of zero. Neither
# matches `^test result` with a non-zero count, so the sum below is the wasm
# count and nothing else.
#
# `--release`, because the stanza below ships a release bundle and that is the
# wasm the page actually loads -- a debug-only gate would miss exactly the
# class of divergence this exists to catch, an `f64` expression the optimiser
# reassociates on one backend. It is also the cheaper of the two here, since it
# shares its build with the bundle instead of adding a second debug one. All
# four combinations were measured on 2026-08-15 and all four agree bit for
# bit: native debug, native release, wasm debug, wasm release.
echo "== viewer: wasm test =="
(cd viewer && wasm-pack test --node --release 2>&1 |
  grep -E "^test result" |
  awk '{s+=$4; f+=$6} END {print "passed="s, "failed="f; if (s<1 || f>0) exit 1}')

# `wasm-pack`, not `cargo build --target wasm32`: the latter proves the crate
# compiles and leaves `viewer/pkg/` exactly as stale as it was. The page loads
# `pkg/`, so a green check with a seven-hour-old bundle is the same failure
# `viewer/serve.py` exists to prevent, one level up -- a viewer showing the
# compiler as it was, indistinguishable from a change that did nothing.
echo "== viewer: wasm bundle =="
(cd viewer && wasm-pack build --target web 2>&1 | tail -1)

echo "OK"
