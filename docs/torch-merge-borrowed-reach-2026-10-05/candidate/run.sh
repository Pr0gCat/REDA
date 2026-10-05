#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != --bounded ]]; then
 exec bash /home/seith/REDA/tools/run_memory_bounded.sh bash /home/seith/REDA/target/qualification/native-rom64-borrowed-reach/run.sh --bounded
fi
cd /home/seith/REDA/target/qualification/native-rom64-borrowed-reach/source
source /home/seith/.cargo/env
ulimit -s 2097152
export RUST_MIN_STACK=2147483648 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=/home/seith/REDA/target/qualification/native-rom64-borrowed-reach/build
python3 ../verify-source.py > ../source-verification.log
set +e
cargo test --offline --locked --lib --no-run > ../build.log 2>&1
result=$?
printf '%s\n' "$result" > ../build.exit
if (( result != 0 )); then exit "$result"; fi
python3 ../bind-binaries.py
result=$?
if (( result != 0 )); then exit "$result"; fi
cargo test --offline --locked --lib compile::tests::torch_merge_accepts_both_branches_of_a_declared_merge_as_g1s_input -- --exact --test-threads=1 --nocapture > ../torch-merge-1.log 2>&1
result=$?
printf '%s\n' "$result" > ../torch-merge-1.exit
if (( result != 0 )); then exit "$result"; fi
if ! rg -q 'test result: ok\. 1 passed; 0 failed;' ../torch-merge-1.log; then exit 65; fi
cargo test --offline --locked --lib compile::tests::torch_merge_still_rejects_the_same_two_repeaters_without_a_declared_merge -- --exact --test-threads=1 --nocapture > ../torch-merge-2.log 2>&1
result=$?
printf '%s\n' "$result" > ../torch-merge-2.exit
if (( result != 0 )); then exit "$result"; fi
if ! rg -q 'test result: ok\. 1 passed; 0 failed;' ../torch-merge-2.log; then exit 65; fi
cargo test --offline --locked --lib compile::tests::torch_merge_rejects_a_foreign_net_that_reaches_the_support -- --exact --test-threads=1 --nocapture > ../torch-merge-3.log 2>&1
result=$?
printf '%s\n' "$result" > ../torch-merge-3.exit
if (( result != 0 )); then exit "$result"; fi
if ! rg -q 'test result: ok\. 1 passed; 0 failed;' ../torch-merge-3.log; then exit 65; fi
cargo test --offline --locked --lib compile::tests::torch_merge_rejects_a_declared_input_whose_net_never_reaches_the_support -- --exact --test-threads=1 --nocapture > ../torch-merge-4.log 2>&1
result=$?
printf '%s\n' "$result" > ../torch-merge-4.exit
if (( result != 0 )); then exit "$result"; fi
if ! rg -q 'test result: ok\. 1 passed; 0 failed;' ../torch-merge-4.log; then exit 65; fi
cargo test --offline --locked --lib compile::tests::torch_merge_accepts_a_correctly_wired_single_input_gate -- --exact --test-threads=1 --nocapture > ../torch-merge-5.log 2>&1
result=$?
printf '%s\n' "$result" > ../torch-merge-5.exit
if (( result != 0 )); then exit "$result"; fi
if ! rg -q 'test result: ok\. 1 passed; 0 failed;' ../torch-merge-5.log; then exit 65; fi
cargo test --offline --locked --lib compile::selection::rom64_selection_diagnostic::ordinary_physical_entry_stage_probe -- --exact --test-threads=1 --nocapture > ../probe.log 2>&1
result=$?
printf '%s\n' "$result" > ../probe.exit
python3 ../verify-source.py >> ../source-verification.log
verify_result=$?
if (( verify_result != 0 )); then exit "$verify_result"; fi
exit "$result"
