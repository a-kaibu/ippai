# Fuzzing

Coverage-guided fuzzing with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
(libFuzzer, nightly). Property-based tests (proptest) are ordinary `cargo test` tests in the
crates (`mod proptests`). The shader translators have their own mutation fuzzer
(`shaderconf --fuzz`, see formal/README.md).

| target | what | must hold |
|---|---|---|
| `x86_jit` | the JIT (koji::jit) against the interpreter: a register state and up to 1 KB of code, translated, validated (wasmparser), run in wasmtime with the dispatcher and kura's block helpers | valid wasm, no trap, and the interpreter reaches the JIT's state (registers, flags, x87/MMX/SSE, memory) |
| `x86_jit64` | the long-mode JIT (koji::jit::long) against the interpreter: sixteen registers and up to 1 KB of x86-64 code run through the dispatcher in regions of 1 to 4 blocks, a module each (nearly every jump, call and return a hop between regions), translated where the dispatcher misses, with only the named registers moved per region or all sixteen | valid wasm, no trap, and the interpreter reaches the JIT's state (registers, RIP, flags, SSE, memory) |
| `x86_interp` | koji::step on any bytes, 32-bit, 64-bit and 16-bit mode | no panic |
| `uuid` | sake::misc::parse_uuid (UuidFromString) on any UTF-16 text | Some exactly for the 8-4-4-4-12 hex form, printing back as the same text |
| `sid` | sake::security::parse_sid (ConvertStringSidToSid) on any text | 15 sub-authorities and 48-bit authority at most; the printed SID parses back the same |
| `wsprintf` | sake::user::wsprintf_format (wsprintf's dialect) on any UTF-16 text | no more `%` than given, text without `%` unchanged, idempotent |
| `glsl` | glwgsl::check / link (OpenGL GLSL -> WGSL, preprocessor first) on any text (vertex NUL fragment) | no panic; a linked program's uniform blocks have members and hold them |
| `image` | sake::ole::decode (BMP, PNG, JPEG: OleLoadPicture, WIC) | no panic, w x h BGRA out |
| `acm` | sake::acm IMA / MS ADPCM blocks | no panic, all of a block's samples or none |
| `fdi` | sake::cabinet::copy (cabinet.dll FDICopy) on any bytes as a cabinet, stored / MSZIP / LZX folders | no panic; writes only to the handle fdintCOPY_FILE returned and within that file's size; one fdintCLOSE_FILE_INFO per copied file after all its bytes |
| `printf` (crates/crt) | crt::fmt::format, every mode (UCRT option bits, legacy msvcrt), narrow and wide | no panic, output within MAX_OUT, text without % unchanged |
| `scanf` (crates/crt) | crt::scan on any input and format | no panic, writes fit the secure forms' buffers, the count never exceeds the conversions |
| `strto` (crates/crt) | strtod/strtof/strtol/strtoul/strtoll against glibc | same value, end and errno (Windows' errno where it differs: no ERANGE for subnormals) |
| `strings` (crates/crt) | the _s string functions against their models (crt::sec::model) | same errno and buffer |
| `strftime` (crates/crt) | crt::time strftime and mktime on any format and struct tm | no panic, mktime's result in range and gmtime gives back the normalized fields |
| `unpack` (crates/ippai) | `ippai add`'s unpacking (zip, InstallShield, zlib-stream setups and the installers inside; no tools) on any file as .zip or .exe | no panic, no allocation over 16 MiB, nothing over the caps (Limits) written, nothing outside the output folder |

Run (from the repository root):

    rustup toolchain install nightly --profile minimal
    cargo install --locked cargo-fuzz
    fuzz/run.sh 60              # every target 60 s (fuzz/ and crates/*/fuzz/)
    fuzz/run.sh 600 x86_jit     # one target

`fuzz/seeds.py` writes the starting inputs to `fuzz/seeds/` (gcc -m32 output of a few C
functions at several -O levels, small BMP/PNG/JPEG files, ADPCM blocks); `run.sh` calls it.
Corpus (`corpus/`), crash inputs (`artifacts/`) and seeds stay out of git; a crash input
looked into is kept in `regressions/<target>/` (in git, run at the start of every run):
`x86_jit/popf-sets-tf` (the harness's step helper went on in the block after POPF set TF,
unlike kura's, which leaves it) and `x86_jit64/enter-faults-after-push` (ENTER pushing RBP
before a frame pointer it cannot read: the JIT and the interpreter write the same bytes below
RSP; the harness compared the state before the instruction). To look at a crash:

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
