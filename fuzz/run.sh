#!/usr/bin/env bash
# Runs every cargo-fuzz target in fuzz/ and crates/*/fuzz/ for SECONDS each (default 60),
# from that directory's corpus/<target> (kept between CI runs) and seeds/<target> (made by its
# seeds.py when there is one). Crash inputs land in <dir>/artifacts/<target>/; the script
# goes on with the other targets and fails at the end if any target failed.
#   fuzz/run.sh [seconds] [target ...]
# MERGE=1: also shrink each corpus to the inputs that add coverage.
set -u -o pipefail
cd "$(dirname "$0")/.."
secs=${1:-60}
shift || true
only=" $* "
failed=()
for dir in fuzz crates/*/fuzz; do
  [ -f "$dir/Cargo.toml" ] || continue
  (cd "$dir" && { [ ! -f seeds.py ] || python3 -I seeds.py; } && cargo +nightly fuzz build -O) || { failed+=("$dir (build)"); continue; }
  for t in $(cd "$dir" && cargo +nightly fuzz list); do
    [ "$only" = "  " ] || [[ "$only" == *" $t "* ]] || continue
    mkdir -p "$dir/corpus/$t" "$dir/artifacts/$t"
    seeds=()
    [ -d "$dir/seeds/$t" ] && seeds=("seeds/$t")
    echo "== $dir $t (${secs}s)"
    (cd "$dir" && cargo +nightly fuzz run -O "$t" "corpus/$t" "${seeds[@]}" -- \
      -max_total_time="$secs" -timeout=20 -rss_limit_mb=4096 -print_final_stats=1 2>&1 | grep -vE '^(#[0-9]+|INFO:|\s+NEW_FUNC)') \
      || failed+=("$dir $t")
    if [ "${MERGE:-0}" = 1 ]; then
      (cd "$dir" && rm -rf "corpus/$t.new" && mkdir "corpus/$t.new" \
        && cargo +nightly fuzz run -O "$t" "corpus/$t.new" "corpus/$t" -- -merge=1 -rss_limit_mb=4096 >/dev/null 2>&1 \
        && rm -rf "corpus/$t" && mv "corpus/$t.new" "corpus/$t")
    fi
  done
done
if [ ${#failed[@]} -gt 0 ]; then
  printf 'failed: %s\n' "${failed[@]}"
  exit 1
fi
