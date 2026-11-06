#!/bin/bash
# Kani proofs (#[cfg(kani)] modules, `#[kani::proof]`) of the crates that have them: bounded model
# checking of the graphics layout arithmetic for every input. Needs `cargo kani` (cargo install
# --locked kani-verifier && cargo kani setup).
set -euo pipefail
cd "$(dirname "$0")/.."
for c in kura; do
  (cd "crates/$c" && cargo kani --output-format terse)
done
