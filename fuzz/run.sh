#!/usr/bin/env bash
# Runs every cargo-fuzz target in fuzz/ and crates/*/fuzz/ for SECONDS each (default 60),
# from that directory's corpus/<target> (kept between CI runs) and seeds/<target> (made by its
# seeds.py when there is one). Crash inputs land in <dir>/artifacts/<target>/; the script
# goes on with the other targets and fails at the end if any target failed.
#   fuzz/run.sh [seconds] [target ...]
# MERGE=1: also shrink each corpus to the inputs that add coverage. JOBS (default: the CPU
# count): targets fuzzed at once (each libFuzzer run is one process; every target still gets
# its full SECONDS). Each target's output is printed when it ends.
set -u -o pipefail
cd "$(dirname "$0")/.."
secs=${1:-60}
shift || true
only=" $* "
jobs=${JOBS:-$(nproc)}
logs=$(mktemp -d)
failed=()

one() { # dir target seeds...
  local dir=$1 t=$2; shift 2
  (cd "$dir" && cargo +nightly fuzz run -O "$t" "corpus/$t" "$@" -- \
    -max_total_time="$secs" -timeout=20 -rss_limit_mb=4096 -print_final_stats=1 2>&1 | grep -vE '^(#[0-9]+|INFO:|\s+NEW_FUNC)') \
    || return 1
  if [ "${MERGE:-0}" = 1 ]; then
    (cd "$dir" && rm -rf "corpus/$t.new" && mkdir "corpus/$t.new" \
      && cargo +nightly fuzz run -O "$t" "corpus/$t.new" "corpus/$t" -- -merge=1 -rss_limit_mb=4096 >/dev/null 2>&1 \
      && rm -rf "corpus/$t" && mv "corpus/$t.new" "corpus/$t")
  fi
  return 0
}

# (finished runs: print the log, note a failure)
reap() {
  for f in "$logs"/*.rc; do
    [ -e "$f" ] || continue
    local b=${f%.rc}
    cat "$b.log"
    [ "$(cat "$f")" = 0 ] || failed+=("$(cat "$b.name")")
    rm -f "$f" "$b.log" "$b.name"
  done
}

n=0
for dir in fuzz crates/*/fuzz; do
  [ -f "$dir/Cargo.toml" ] || continue
  (cd "$dir" && { [ ! -f seeds.py ] || python3 -I seeds.py; } && cargo +nightly fuzz build -O) || { failed+=("$dir (build)"); continue; }
  for t in $(cd "$dir" && cargo +nightly fuzz list); do
    [ "$only" = "  " ] || [[ "$only" == *" $t "* ]] || continue
    mkdir -p "$dir/corpus/$t" "$dir/artifacts/$t"
    seeds=()
    [ -d "$dir/seeds/$t" ] && seeds=("seeds/$t")
    # (inputs that once failed, kept in git: run first, every time)
    [ -d "$dir/regressions/$t" ] && seeds+=("regressions/$t")
    while [ "$(jobs -rp | wc -l)" -ge "$jobs" ]; do wait -n || true; reap; done
    n=$((n + 1))
    k=$logs/$n
    echo "$dir $t" > "$k.name"
    echo "== $dir $t (${secs}s)"
    ( { echo "== $dir $t (${secs}s)"; one "$dir" "$t" "${seeds[@]}"; } > "$k.log" 2>&1; echo $? > "$k.rc.tmp"; mv "$k.rc.tmp" "$k.rc" ) &
  done
done
wait
reap
rm -rf "$logs"
if [ ${#failed[@]} -gt 0 ]; then
  printf 'failed: %s\n' "${failed[@]}"
  exit 1
fi
