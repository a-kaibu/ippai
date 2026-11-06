# Formal verification of the graphics paths

`.github/workflows/formal.yml` runs all of it on every push.

| What | How | Run |
| --- | --- | --- |
| Command queue between guest threads and the render thread (Direct3D 8/9/10/11, OpenGL): batches, generations, uploads straight from guest memory (`WRITE_TEXTURE_MEM`, `detach_mem`, `wait_done`, deferred frees) | TLA+ `tla/GpuQueue.tla`: no upload the page runs sees pixels the guest changed after recording it; waits end | `formal/run.sh` (TLC from [tlc-static](https://github.com/yashikota/tlc-static)) |
| Readbacks (GetRenderTargetData, Map, glReadPixels): give-up vs. the page's copy | TLA+ `tla/GpuReadback.tla`: the page never writes `dst` after its waiter returned; nothing leaks | 〃 |
| Movie slots (WebCodecs frames, `video.rs`) | TLA+ `tla/VideoSlots.tla`: no use after free, no torn read, closed streams are dropped | 〃 |
| Texel conversion (`d3d9/format.rs`, `d3d11/format.rs`) against references written from the Direct3D docs | exhaustive tests: every texel of every 16- and 8-bit format, every DXT endpoint pair per channel in both modes, every DXT5 alpha pair and code, every f32 → f16 (against `half`, IEEE round to nearest even) | `cargo test -p kura --release format` |
| UNORM expansion, pitch/row arithmetic of every D3D9 and DXGI format, window rectangle algebra | Kani proofs (`#[cfg(kani)] mod proofs`) for all inputs in the API's limits | `formal/kani.sh` |
| unsafe conversion loops on exactly sized buffers, movie slots across threads, padded GPU uploads | Miri | `formal/miri.sh` |
| Shader translation (HLSL → DXBC / D3D9 bytecode, D3D9 assembly, GLSL → WGSL): what the shaders compute on a real WebGPU (Chromium's Dawn) against the D3D11.3 functional spec, the D3D9 instruction reference, HLSL constant packing, GLSL 1.20 and std140 | `crates/shaderconf` writes the cases (shader, uniform data per pixel, expected pixel), `tests/shaders.mjs` draws them | `cargo run -q -p shaderconf > c.json && node tests/shaders.mjs c.json` |

`tla/models.txt` lists the configurations. Two of them must fail, as records of known limits:

- `GpuQueueTimeouts.cfg`: when `wait_done` gives up (its 1 s deadline, or a page that has not
  polled for 3 s) the guest may change pixels the page is still uploading: a torn texture for a
  frame, never a memory error (guest memory stays mapped). The waits give up to avoid a hang.
- `VideoSlotsFlag.cfg`: `reading` as a flag (as it was) lets two guest threads reading the same
  slot free it under each other; `reading` is a count now.

Found and fixed with these: the movie slot flag above; `f16_bits` rounded ties up (4535 of every
7th f32 differed from IEEE round to nearest even); 5- and 6-bit channels were expanded by bit
replication, off by one from D3D's UNORM rule for 14 values.

Found and fixed by the shader cases: GLSL programs with a `uniform mat2` did not translate at all
(naga has no std140 layout for 2-row matrices: kept as 4-row ones now); `uniform float/vec2/vec3
x[n]` were read 4/8/12 bytes apart while their data was 16 apart (std140; now vec4 arrays);
HLSL `firstbithigh` returned the bit index from the top; the D3D9 assembler turned Microsoft's
`if_gt` / `break_gt` into plain `if` / `break` (constant false); DXBC `ne` gave false for NaN on
WebGPU, `ftoi`/`ftou` saturated short of INT_MAX / UINT_MAX and NaN, `f32tof16` of large values
was NaN, `log` of 0 and negatives returned log2(|x|).
Accepted where WGSL leaves the result open and no game depends on it: `frc` of infinities and
NaN, `exp` of NaN, denormal results flushed to zero.
