#!/bin/sh
# Runs a command on a machine slowed down to about an Android phone's speed (for perf work
# without the phone): HOGS busy loops per core (default 1) compete with it at the same
# priority, so every thread of the browser (Wine's workers, the GPU process, swiftshader) gets
# 1/(HOGS+1) of a core when it runs. One hog per core = about 2x slower, roughly the user's
# phone against this container (bench.html: phone wasm ~800 / JIT ~1000 ms; here 540 / 600,
# with this 860-1120 / 1050-1130).
#
#   tests/android-like.sh node tests/play.mjs URL OUT ...
HOGS=${HOGS:-1}
pids=""
for c in $(seq 0 $(($(nproc) - 1))); do
  for _ in $(seq 1 "$HOGS"); do
    # (each in its own session: the browser runs in its own, and sessions share the CPU as
    # groups, see sched_autogroup)
    setsid taskset -c "$c" sh -c 'while :; do :; done' &
    pids="$pids $!"
  done
done
trap 'kill $pids 2>/dev/null' EXIT INT TERM
"$@"
