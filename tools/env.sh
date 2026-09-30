# Source from Bash: source tools/env.sh
reda_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$HOME/.cargo/env"
source "$reda_root/.venv/bin/activate"
export REDA_PYTHON="$reda_root/.venv/bin/python"
export CARGO_TARGET_DIR="$reda_root/target"
export YOWASP_CACHE_DIR="$reda_root/target/yowasp-cache"
# YoWASP mounts a private /tmp; keep frontend scratch files visible under the repo.
export TMPDIR="$reda_root/target/tmp"
mkdir -p "$TMPDIR"
unset reda_root
