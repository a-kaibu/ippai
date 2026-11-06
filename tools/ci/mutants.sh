#!/bin/bash
# Mutation testing with cargo-mutants (.cargo/mutants.toml, profile "mutants" in Cargo.toml).
#   tools/ci/mutants.sh diff BASE   mutants in the Rust lines changed since BASE
#   tools/ci/mutants.sh gate        every mutant of the files in tools/ci/mutants-gate.txt
# Writes target/mutants.out and a Markdown report (also to $GITHUB_STEP_SUMMARY). A missed
# mutant fails the run only in a gate file (those have no missed mutants left: a new one is a
# test gap of the change); elsewhere it is reported. A diff of more than MUTANTS_MAX (400)
# mutants runs a round-robin sample of them.
set -euo pipefail
cd "$(dirname "$0")/../.."
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=$PWD/tools/ci/memcap.sh
JOBS=${MUTANTS_JOBS:-2}
MAX=${MUTANTS_MAX:-400}
mkdir -p target
args=()
case "${1:-}" in
  diff)
    git diff "$2" HEAD -- 'crates/*.rs' > target/mutants.diff
    if [ ! -s target/mutants.diff ]; then echo "mutants: no Rust changes since $2"; exit 0; fi
    args=(--in-diff target/mutants.diff)
    n=$(cargo mutants "${args[@]}" --list | wc -l)
    echo "mutants: $n in the diff"
    [ "$n" -gt 0 ] || exit 0
    if [ "$n" -gt "$MAX" ]; then
      k=$(( (n + MAX - 1) / MAX ))
      echo "mutants: sampling 1/$k"
      args+=(--shard "0/$k" --sharding round-robin)
    fi
    ;;
  gate)
    while read -r f; do
      case "$f" in "" | "#"*) ;; *) args+=(-f "$f") ;; esac
    done < tools/ci/mutants-gate.txt
    ;;
  *) echo "usage: $0 diff BASE | gate" >&2; exit 2 ;;
esac
rc=0
cargo mutants "${args[@]}" -j "$JOBS" -o target || rc=$?
# 0: all caught, 2: missed, 3: timeouts; others (baseline failed, usage) are errors
case $rc in 0 | 2 | 3) ;; *) exit $rc ;; esac
python3 tools/ci/mutants-report.py target/mutants.out tools/ci/mutants-gate.txt tools/ci/mutants-known.txt | tee target/mutants.md
