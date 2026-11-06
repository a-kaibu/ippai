# Fuzzing

Coverage-guided fuzzing with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
(libFuzzer, nightly). Property-based tests (proptest) are ordinary `cargo test` tests in the
crates (`mod proptests`). The shader translators have their own mutation fuzzer
(`shaderconf --fuzz`, see formal/README.md).

| target | what | must hold |
|---|---|---|
| `x86_jit` | the JIT (koji::jit) against the interpreter: a register state and up to 1 KB of code, translated, validated (wasmparser), run in wasmtime with the dispatcher and kura's block helpers | valid wasm, no trap, and the interpreter reaches the JIT's state (registers, flags, x87/MMX/SSE, memory) |
| `x86_interp` | koji::step on any bytes, 32-bit, 64-bit and 16-bit mode | no panic |
| `image` | sake::ole::decode (BMP, PNG, JPEG: OleLoadPicture, WIC) | no panic, w x h BGRA out |
| `acm` | sake::acm IMA / MS ADPCM blocks | no panic, all of a block's samples or none |

Run (from the repository root):

    rustup toolchain install nightly --profile minimal
    cargo install --locked cargo-fuzz
    fuzz/run.sh 60              # every target 60 s (fuzz/ and crates/*/fuzz/)
    fuzz/run.sh 600 x86_jit     # one target

`fuzz/seeds.py` writes the starting inputs to `fuzz/seeds/` (gcc -m32 output of a few C
functions at several -O levels, small BMP/PNG/JPEG files, ADPCM blocks); `run.sh` calls it.
Corpus (`corpus/`), crash inputs (`artifacts/`) and seeds stay out of git. To look at a crash:

    cd fuzz && cargo +nightly fuzz run -O x86_jit artifacts/x86_jit/crash-...

What `x86_jit` leaves out on purpose (each a choice of the JIT, not a bug):

- Status flags at an exception, and flags the code overwrites before reading them: the JIT
  computes flags only where something reads them, also when it leaves a block early for the
  interpreter. After the JIT stops, both states run on in the interpreter for 64 more
  instructions and must agree there (a dead flag that differs never changes anything).
- x87 tags of a stack that underflows (registers compared where both runs have them valid).
- Code touching memory outside the 256 KB window: not compared (the JIT has no faults, its
  wasm memory is all 4 GB).

CI: `.github/workflows/fuzz.yml`, a minute per target on every push to main and five minutes
(corpus merged) daily; the corpus is kept in the Actions cache, crash inputs are uploaded as
the `fuzz-crashes` artifact.
