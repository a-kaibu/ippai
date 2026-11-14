#!/bin/bash
# Verus proofs (formal/verus/*.rs): unbounded, SMT-checked (Z3) proofs of small algorithms of
# masu, written as Verus code with the same steps as the Rust they stand for. VERUS: the
# verus script of a release (default: verus on PATH); formal.yml's "verus" job fetches one.
set -euo pipefail
cd "$(dirname "$0")/.."
V=${VERUS:-verus}
fail=0
for f in formal/verus/*.rs; do
  echo "== $f"
  "$V" "$f" || fail=1
done
exit $fail
