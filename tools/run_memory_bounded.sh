#!/usr/bin/env bash
# Local qualification jobs share one lock and one aggregate memory limit.
set -euo pipefail
if (( $# == 0 )); then
    echo "Usage: $0 command [arguments...]" >&2
    exit 64
fi
reda_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$reda_root/target/qualification"
exec 9>"$reda_root/target/qualification/heavy-task.lock"
if ! flock -n 9; then
    echo "Another REDA qualification job is running; this job was not started." >&2
    exit 75
fi
task_available_kib="$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)"
if [[ ! "$task_available_kib" =~ ^[0-9]+$ ]] || (( task_available_kib < 4194304 )); then
    echo "Less than 4 GiB available; this job was not started." >&2
    exit 75
fi
export CARGO_BUILD_JOBS=1
systemd-run --user --scope --quiet \
    --property=MemoryMax=4294967296 --property=MemorySwapMax=0 -- "$@"
