#!/bin/sh
# Test runner under cargo mutants (CARGO_TARGET_*_RUNNER): a mutant that allocates without end
# (a loop bound turned around) fails at 3 GB of address space instead of taking the machine.
ulimit -v 3000000
exec "$@"
