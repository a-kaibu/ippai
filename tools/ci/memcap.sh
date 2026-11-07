#!/bin/sh
# Test runner under cargo mutants (CARGO_TARGET_*_RUNNER): a mutant that allocates without end
# (a loop bound turned around) fails at 3 GB of address space instead of taking the machine, and
# one that writes without end fails at 2 GB a file (SIGXFSZ) instead of filling the runner's disk
# (#83: two runs ran out of room with 100 GB free at the start).
ulimit -v 3000000
ulimit -f "${MEMCAP_FILE_BLOCKS:-4194304}"
exec "$@"
