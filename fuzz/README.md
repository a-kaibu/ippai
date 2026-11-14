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
| `ws2name` | sake::ws2name (ws2_32 names): IDNA / Punycode of any text, a hosts file, the hostent size bound | no panic; Punycode ASCII and only for labels of at most 63 characters; a hostent within MAXGETHOSTSTRUCT (1024) |
| `vfs_pages` | kura::pages::Pages (files written in the page's memory, #234) against a Vec: writes, cuts and lengthenings near page edges | the same bytes from any offset, its length, at most its pages' worth held |
| `uuid` | sake::misc::parse_uuid (UuidFromString) on any UTF-16 text | Some exactly for the 8-4-4-4-12 hex form, printing back as the same text |
| `xfile` | sake::xof (d3dxof.dll's .x files: text, binary, MSZIP-compressed) on any bytes after each header, with templates registered (one reusing member names for array sizes) | no panic; each object's members back to back over its data, strings inside it; RegisterTemplates adds all of a file's templates or none |
| `sid` | sake::security::parse_sid (ConvertStringSidToSid) on any text | 15 sub-authorities and 48-bit authority at most; the printed SID parses back the same |
| `wsprintf` | sake::user::wsprintf_format (wsprintf's dialect) on any UTF-16 text | no more `%` than given, text without `%` unchanged, idempotent |
| `rtf` | sake::rtf::to_text (a rich edit's EM_STREAMIN with SF_RTF) on any bytes | no panic; the text written back as RTF (from_text) reads as the same text |
| `glsl` | glwgsl::check / link (OpenGL GLSL -> WGSL, preprocessor first) on any text (vertex NUL fragment) | no panic (sake is panic=abort: naga's front end panics are fixed in vendor/naga); a linked program's uniform blocks have members and hold them |
| `hlsl` | hlslc (D3DXCompileShader / D3DCompile / D3DXCreateEffect HLSL text, D3DXAssembleShader assembly) on any text, sm2-5 targets, then dxwgsl on what it compiles | no panic; its sm2/3 bytecode parses as a D3D9 shader, its effects read back in sake::d3dx9fx |
| `effect` | sake::d3dx9fx::check (D3DXCreateEffect reading a compiled fx_2_0 effect) on any bytes, with and without the effect tag | no panic, no allocation beyond the file's size; every parameter inside its top-level value |
| `undname` | sake::undname (__unDName, UnDecorateSymbolName, type_info::name) on any text and flags | no panic, also on its own output |
| `image` | sake::ole::decode (BMP, PNG, JPEG: OleLoadPicture, WIC) | no panic, w x h BGRA out |
| `dds` | sake::dds::parse (DDS headers D3DX reads: 2D, cube maps, DXTn) | no panic, every face's levels inside the data, back to back |
| `acm` | sake::acm IMA / MS ADPCM blocks | no panic, all of a block's samples or none |
| `kern_pipes` | sake::kern pipes: any sequence of byte writes (resumed while they wait), reads, peeks, messages, PIPE_NOWAIT, a closed reader, and the state written to bytes and read back (another process) | what is read is what was written, in order; at most the reader's buffer (or 4096) waits; a write is done only once what waits fits; the state reads back as itself |
| `vfs_journal` | kura's shared namespace (vfs/shared.rs): any bytes as a journal entry, then as three processes' batches of puts, writes, size changes, opens, closes, unlinks, mkdirs through one buffer (sometimes too small) | a refused entry changes nothing; after each publish every process has exactly the publisher's namespace, handles valid, free slots the empty ones |
| `fdi` | sake::cabinet::copy (cabinet.dll FDICopy) on any bytes as a cabinet, stored / MSZIP / LZX folders | no panic; writes only to the handle fdintCOPY_FILE returned and within that file's size; one fdintCLOSE_FILE_INFO per copied file after all its bytes |
| `loaded_image` | sake::imageload::layout (imagehlp MapAndLoad / ImageLoad) on any bytes as a PE file | no panic; a layout only with MZ, PE\0\0 at e_lfanew, a PE32 / PE32+ optional header and the whole section table in the file |
| `dxbc` | dxwgsl::translate / translate_compute on any DXBC (vertex, pixel, compute, a vertex shader with the pixel shader it feeds) | no panic; what it takes is WGSL naga parses and validates |
| `d3d9_shader` | dxwgsl's Direct3D 9 shaders (vs/ps 1.x-3.0 tokens) with alpha test, fog and 1-4 targets | no panic; a shader it parses gives WGSL naga validates |
| `apitrace` | apitrace's snappy, trace container and replay stream (the game traces) | no panic; snappy gives back what it compressed |
| `msi` | msidb: an MSI database (compound file) with every field of every table and its streams read; SQL statements; condition expressions | no panic |
| `jit_saved` | koji's saved translations index (from the server or a damaged file) | no panic; a whole index reads the same alone and in a bundle |
| `mp3_frames` | sake's MP3 frame splitter (msacm32's MP3 codec) | the same frames however the stream is cut; each a whole frame its header describes |
| `mpeg1` | crates/mpeg1: MPEG-1 system stream demuxer, video and layer II audio decoders (the DirectShow MPEG-1 splitter) | no panic; the same packets, pictures and samples however the stream is cut; pictures the size of their sequence header |
| `asf` | crates/asf: the ASF demuxer of sake's Windows Media readers (wmvcore) on any bytes as a file and as a packet; seeking | no panic; no object over asf::MAX_OBJECT |
| `wma` | crates/wmcodec::wma: WMA 1 / 2 (FFmpeg's decoder ported) with any stream parameters and packets | no panic; every packet gives whole frames, as many per channel |
| `wmv` | crates/wmcodec::msmpeg4: MS-MPEG4 v3 / WMV 1 / WMV 2 (FFmpeg's decoders ported) with any size, extradata and pictures | no panic; pictures of the stream's size |
| `wmdec` | sake's Windows Media decoders (wmdec: the codec for any BITMAPINFOHEADER / WAVEFORMATEX, pictures and sound into an RGB24 / RGB32 / PCM output of any size) | no panic; pictures the size of their output |
| `vorbis_hook` | sake::vorbis_hook::locate_in (the Vorbis decoder found inside a game's exe) on any bytes as a module image | no panic; the functions it finds lie in the image |
| `ne_header` | sake::w16::check_ne (the Win16 NE loader's header readers: module name, entry points, imported names, exports) | no panic, no hang |
| `audiofile` | sake::audiofile (WAVE, MP4/M4A, Ogg) | no panic |
| `formats` | .lnk, event log records and .evt files, PE resource trees, type libraries, CryptStringToBinary / CryptBinaryToString | no panic; what decodes encodes back to the same (links, .evt, resource trees, base64 forms) |
| `printf` (crates/crt) | crt::fmt::format, every mode (UCRT option bits, legacy msvcrt), narrow and wide | no panic, output within MAX_OUT, text without % unchanged |
| `scanf` (crates/crt) | crt::scan on any input and format | no panic, writes fit the secure forms' buffers, the count never exceeds the conversions |
| `strto` (crates/crt) | strtod/strtof/strtol/strtoul/strtoll against glibc | same value, end and errno (Windows' errno where it differs: no ERANGE for subnormals) |
| `strings` (crates/crt) | the _s string functions against their models (crt::sec::model) | same errno and buffer |
| `strftime` (crates/crt) | crt::time strftime and mktime on any format and struct tm | no panic, mktime's result in range and gmtime gives back the normalized fields |
| `unpack` (crates/ippai) | `ippai add`'s unpacking (zip, InstallShield, zlib-stream setups and the installers inside; no tools) on any file as .zip or .exe | no panic, no allocation over 16 MiB, nothing over the caps (Limits) written, nothing outside the output folder |
| `asar` (crates/ippai) | `ippai pack`'s Electron app.asar header (asar::parse) on any file | no panic, every file a plain relative path inside the archive's bytes |
| `evb` (crates/ippai) | `ippai pack`'s Enigma Virtual Box unpacking (evb::locate/parse/extract, aPLib depack) on any file | no panic, every file a plain relative path inside the exe's bytes, unpacked sizes as the box says and within the cap |

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
unlike kura's, which leaves it), `d3d9_shader/*` (D3D9 shaders dxwgsl turned into invalid WGSL: fog mixing the uniform's u32 words, texldd in ps_1_x, a texcoord op writing a temp, a constant named past the relative range, a matrix of a0, a register declared twice) and `x86_jit64/enter-faults-after-push` (ENTER pushing RBP
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
