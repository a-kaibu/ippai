#!/bin/bash
# Kani proofs (#[cfg(kani)] modules, `#[kani::proof]`) of the crates that have them: bounded model
# checking of the graphics layout arithmetic, of the Vorbis bit reader and floor curve, and of
# the C runtime's _s functions and itoa (crates/crt), of the MPEG-1 bit reader (crates/mpeg1), and of koji's EFLAGS (the eager helpers
# and the JIT's lazy flags against the SDM's definitions), for every input. Needs `cargo kani`
# (cargo install --locked kani-verifier && cargo kani setup).
set -euo pipefail
cd "$(dirname "$0")/.."
for c in kura vorbis crt koji mpeg1; do
  (cd "crates/$c" && cargo kani --output-format terse)
done
