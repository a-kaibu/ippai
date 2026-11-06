# Formal verification of the graphics paths

`.github/workflows/formal.yml` runs all of it on every push.

| What | How | Run |
| --- | --- | --- |
| Command queue between guest threads and the render thread (Direct3D 8/9/10/11, OpenGL): batches, generations, uploads straight from guest memory (`WRITE_TEXTURE_MEM`, `detach_mem`, `wait_done`, deferred frees) | TLA+ `tla/GpuQueue.tla`: no upload the page runs sees pixels the guest changed after recording it; waits end | `formal/run.sh` (TLC from [tlc-static](https://github.com/yashikota/tlc-static)) |
| Readbacks (GetRenderTargetData, Map, glReadPixels): give-up vs. the page's copy | TLA+ `tla/GpuReadback.tla`: the page never writes `dst` after its waiter returned; nothing leaks | 〃 |
| Movie slots (WebCodecs frames, `video.rs`) | TLA+ `tla/VideoSlots.tla`: no use after free, no torn read, closed streams are dropped | 〃 |
| The executor (`render/d3d.rs`) against WebGPU: every call kura makes checked against the spec's validation rules, and its optimizations (clears deferred into loadOps, stencil cleared and discarded, batches the same as the last one not run, Siglus segments drawn again only where they changed) against plain execution | `render/model.rs`, a model of WebGPU (validation, an abstract meaning of every command: texel terms, queue writes at once, commands at submit, `discard` leaves zeros); `render/d3d/model_tests.rs` runs random Direct3D 9, Direct3D 11, OpenGL/Direct2D-style command streams optimized and plain: the same frames and readbacks, no WebGPU error | `IPPAI_MODEL_SEEDS=N cargo test --release -p kura --lib optimizations_draw_the_same` |
| The rule of `repeats()` (when a batch may be skipped) | TLA+ `tla/RepeatSkip.tla`: for every batch of up to 3 commands (clears, draws with and without blending, writes, copies, fills; partial boxes, an array texture) from any contents, "repeats" implies running it twice leaves what once did | `formal/run.sh` |
| Texel conversion (`d3d9/format.rs`, `d3d11/format.rs`) against references written from the Direct3D docs | exhaustive tests: every texel of every 16- and 8-bit format, every DXT endpoint pair per channel in both modes, every DXT5 alpha pair and code, every f32 → f16 (against `half`, IEEE round to nearest even) | `cargo test -p kura --release format` |
| UNORM expansion, pitch/row arithmetic of every D3D9 and DXGI format, window rectangle algebra | Kani proofs (`#[cfg(kani)] mod proofs`) for all inputs in the API's limits | `formal/kani.sh` |
| Direct3D 9/11 and OpenGL viewports and scissors (`rects9`, `rects11` in `render/d3d.rs`) | Kani: for every target size and every viewport, depth range and scissor (NaN and infinities too), what reaches setViewport / setScissorRect is what WebGPU accepts: inside the target as WebGPU adds it up, depths in 0..1 | `formal/kani.sh` |
| unsafe conversion loops on exactly sized buffers, movie slots across threads, padded GPU uploads | Miri | `formal/miri.sh` |
| Shader translation (HLSL → DXBC / D3D9 bytecode, D3D9 assembly, GLSL → WGSL): what the shaders compute on a real WebGPU (Chromium's Dawn) against the D3D11.3 functional spec, the D3D9 instruction reference, HLSL constant packing, GLSL 1.20 and std140 | `crates/shaderconf` writes the cases (shader, uniform data per pixel, expected pixel), `tests/shaders.mjs` draws them | `cargo run -q -p shaderconf > c.json && node tests/shaders.mjs c.json` |

`tla/models.txt` lists the configurations. Four of them must fail, as records of known limits:

- `GpuQueueTimeouts.cfg`: when `wait_done` gives up (its 1 s deadline, or a page that has not
  polled for 3 s) the guest may change pixels the page is still uploading: a torn texture for a
  frame, never a memory error (guest memory stays mapped). The waits give up to avoid a hang.
- `VideoSlotsFlag.cfg`: `reading` as a flag (as it was) lets two guest threads reading the same
  slot free it under each other; `reading` is a count now.

- `RepeatSkipOld.cfg`: the rule before the model tests: a counterexample (below).
- `RepeatSkipUseful.cfg`: the rule does skip some batches (it is not vacuously safe).

Found and fixed by the WebGPU model: `repeats()` skipped a batch that sampled a texture and then
wrote it from the blob, one that sampled a texture before clearing it, and one that cleared one
layer of an array texture and drew into another (clears were by texture); a pass begun before a
Siglus segment ended let later draws keep the segment's stamp (the next frame redrew only part of
a changed target); deferred clears did not mark the texture changed for draws sampling it;
Direct3D 11 depth buffers no draw had used were cleared to 1 by the optimized passes while plain
ones loaded WebGPU's zeros (they start at 1 now in both).

Kani found: a viewport whose x + width, rounded in f32, came out past the target by one ulp
(a WebGPU validation error, the draw lost); the D3D9 path passed MinZ/MaxZ outside 0..1 and NaN
through. The model tests found StretchRect and GetRenderTargetData with boxes or levels past the
texture, and a copy of depth24plus-stencil8, going to WebGPU as invalid copies.
The state mapping tests (`every_sampler_state_is_valid_webgpu`, `every_pipeline_state_is_valid_webgpu`,
`every_texture_kind_is_valid_webgpu`: every D3D9/D3D11 sampler, blend, depth, raster and texture
state the games can set, through the model's validation) found: a strip index format on list
topologies; blending on formats WebGPU cannot blend (integer, r32float); render targets of
formats it cannot render (snorm, rg11b10 without the feature) and snorm textures made with
RENDER_ATTACHMENT; write masks past 0xf; mip counts past the size; depth textures of 3D or cube
kinds as attachments.

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
