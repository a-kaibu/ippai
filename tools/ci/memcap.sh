#!/bin/sh
# Test runner under cargo mutants (CARGO_TARGET_*_RUNNER): a mutant that allocates without end
# (a loop bound turned around) fails at 3 GB of address space instead of taking the machine, and
# one that writes without end fails at 2 GB a file (SIGXFSZ) instead of filling the runner's disk
# (#83: two runs ran out of room with 100 GB free at the start).
# glibc otherwise retains a large virtual arena per test/helper thread: the unmutated
# baseline hit RLIMIT_AS with only ~300 MB resident. Bound those reservations, not the
# test's coverage; the 3 GB address-space and 2 GB file limits remain unchanged.
export MALLOC_ARENA_MAX="${MALLOC_ARENA_MAX:-2}"
ulimit -v 3000000
ulimit -f "${MEMCAP_FILE_BLOCKS:-4194304}"
exec "$@"
