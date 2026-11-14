# Formal verification of the graphics paths

`.github/workflows/formal.yml` runs all of it on every push.

| What | How | Run |
| --- | --- | --- |
| Command queue between guest threads and the render thread (Direct3D 8/9/10/11, OpenGL): batches, generations, uploads straight from guest memory (`WRITE_TEXTURE_MEM`, `detach_mem`, `wait_done`, deferred frees) | TLA+ `tla/GpuQueue.tla`: no upload the page runs sees pixels the guest changed after recording it; waits end | `formal/run.sh` (TLC from [tlc-static](https://github.com/yashikota/tlc-static)) |
| Readbacks (GetRenderTargetData, Map, glReadPixels): give-up vs. the page's copy | TLA+ `tla/GpuReadback.tla`: the page never writes `dst` after its waiter returned; nothing leaks | 〃 |
| Movie slots (WebCodecs frames, `video.rs`) | TLA+ `tla/VideoSlots.tla`: no use after free, no torn read, closed streams are dropped | 〃 |
| Save states (`kura/src/snap.rs`): guest threads parking at safe points, host threads idle/busy, the coordinator pausing, waiting for the parked and the idle ones, READY while the page copies, resume_all | TLA+ `tla/SnapPause.tla` (3 guest and 2 host threads, 2 saves): while the page copies no thread runs, holds GATE or woke from its wait (STIRS unchanged); every save ends and every thread works again | `formal/run.sh` |
| The executor (`render/d3d.rs`) against WebGPU: every call kura makes checked against the spec's validation rules, and its optimizations (clears deferred into loadOps, stencil cleared and discarded, batches the same as the last one not run, Siglus segments drawn again only where they changed) against plain execution | `render/model.rs`, a model of WebGPU (validation, an abstract meaning of every command: texel terms, queue writes at once, commands at submit, `discard` leaves zeros); `render/d3d/model_tests.rs` runs random Direct3D 9, Direct3D 11, OpenGL/Direct2D-style command streams optimized and plain: the same frames and readbacks, no WebGPU error | `IPPAI_MODEL_SEEDS=N cargo test --release -p kura --lib optimizations_draw_the_same` |
| The rule of `repeats()` (when a batch may be skipped) | TLA+ `tla/RepeatSkip.tla`: for every batch of up to 3 commands (clears, draws with and without blending, writes, copies, fills; partial boxes, an array texture) from any contents, "repeats" implies running it twice leaves what once did | `formal/run.sh` |
| An IMFMediaEngine's life (`sake/src/mf.rs`: en_release, en_shutdown, its events thread asleep on EVENTS_CV, `retire`) | TLA+ `tla/MfEngineLife.tla`: the block is freed once, only when the game let go and the thread ended, and always then; a thread asleep at the shutdown wakes | `formal/run.sh`; proptest `mf::tests::engines_go_once_and_only_when_unheld` |
| A streaming thread's calls into the game's objects and a Stop (`waits/src/calls.rs`, the MPEG-1 splitter in `sake/src/dshow_mpeg.rs`, #230) | TLA+ `tla/StopCalls.tla`: once Stop returned (and the game destroyed the object) no call into it is in flight; loom `waits/tests/loom.rs` (`calls_*`) the same over the Rust code | `formal/run.sh`, `formal/loom.sh` |
| Direct3D 9 and 11 object lifetimes (`d3d9/mod.rs`, `d3d11/mod.rs`): reference counts, what the device state, state blocks, views and swap chains hold, Reset, ResizeBuffers, ClearState | TLA+ `tla/ComRefs.tla`, the API's rules (an object lives while the application, a live holder or a live binding reaches it; AddRef/Release return the public count); `d3d9/trace_tests.rs` and `d3d11/trace_tests.rs` make random calls through the real entry points and write each run as a trace; TLC checks every call's result, returned object and the set of live objects against ComRefs (`tla/ComRefsTrace.tla`) | `formal/trace.sh` |
| Direct3D 9 surface locks and copies (`d3d9/surface.rs`, `device.rs`): LockRect/UnlockRect (read-only, discard, partial rects), ColorFill, UpdateSurface, StretchRect, GetRenderTargetData, and what the page reads back | TLA+ `tla/Locks.tla`: which writes each 4x4 cell shows (a lock sees the last write, fill or GPU copy; the page sees what was uploaded); `d3d9/lock_tests.rs` drives the real entry points with the page's executor on the WebGPU model in the same thread and writes each run as a trace (`tla/LocksTrace.tla`) | `formal/trace.sh` |
| Direct3D 11 Map/Unmap (`d3d11/mod.rs`): map types per usage (staging read/write, dynamic discard/no-overwrite), UpdateSubresource, ClearRenderTargetView, CopyResource, CopySubresourceRegion, readbacks | TLA+ `tla/Maps.tla`: what a Map shows, which map types each resource takes; `d3d11/map_tests.rs` traces (`tla/MapsTrace.tla`) | 〃 |
| OpenGL object names (`gl/mod.rs`): glGen*/glDelete*/glBind*/glIs* for textures, buffers, framebuffers, renderbuffers, attachments, binding queries, glGetError | TLA+ `tla/GlNames.tla` (GL 3.0 / ES 2.0 rules: generated names are unused, an object exists from its first bind, delete unbinds and detaches, errors are sticky until glGetError); `gl/names_tests.rs` traces (`tla/GlNamesTrace.tla`) | 〃 |
| Direct2D drawing state (`sake/src/d2d.rs`, `Drawing`): BeginDraw/EndDraw/Flush, drawing calls, Push/PopAxisAlignedClip | TLA+ `tla/D2dDraw.tla` (D2DERR_WRONG_STATE outside drawing and for a second BeginDraw, POP_CALL_DID_NOT_MATCH_PUSH, PUSH_POP_UNBALANCED at EndDraw); checked on its own (`D2dDrawCheck`) and against traces of the real state (`d2d/draw_tests.rs`) | `formal/run.sh`, `formal/trace.sh` |
| The bytecode semantics of shader translation, instruction by instruction | `crates/shaderconf/src/ref_dxbc.rs` and `ref_d3d9.rs`: reference interpreters of shader model 4/5 and ps_2_0..3_0, written from the D3D11.3 functional spec and the D3D9 instruction reference with their own decoders (nothing shared with dxwgsl). Checked against every case's expected pixels (`interpreters_give_what_the_cases_expect`); then the oracle: the cases' bytecode mutated, each mutant dxwgsl takes and the interpreter runs with a result every implementation must give (nothing out of range, no infinity, NaN or denormal on the way) becomes a case whose expected pixels are the interpreter's, run on WebGPU | `cargo test -p shaderconf`; `shaderconf --oracle 20000 7 > o.json && node tests/shaders.mjs o.json` |
| Texel conversion (`d3d9/format.rs`, `d3d11/format.rs`) against references written from the Direct3D docs | exhaustive tests: every texel of every 16- and 8-bit format, every DXT endpoint pair per channel in both modes, every DXT5 alpha pair and code, every f32 → f16 (against `half`, IEEE round to nearest even) | `cargo test -p kura --release format` |
| UNORM expansion, pitch/row arithmetic of every D3D9 and DXGI format, window rectangle algebra | Kani proofs (`#[cfg(kani)] mod proofs`) for all inputs in the API's limits | `formal/kani.sh` |
| Direct3D 9/11 and OpenGL viewports and scissors (`rects9`, `rects11` in `render/d3d.rs`) | Kani: for every target size and every viewport, depth range and scissor (NaN and infinities too), what reaches setViewport / setScissorRect is what WebGPU accepts: inside the target as WebGPU adds it up, depths in 0..1 | `formal/kani.sh` |
| Win32's user-mode locks on guest memory (`crates/waits`, used by sake's `k32`): critical sections (with the guest's own `lock xchg` leave of `fast.rs`), SRW locks, condition variables over both, INIT_ONCE (ExecuteOnce, Begin/Complete), WaitOnAddress, and their stand-still for save states | loom: every interleaving up to 3 preemptions (4 daily) with loom's weak-memory model and no timeouts on the safety-net waits: what each lock guards is never raced, counts come out exact, no wakeup is lost, readers share, INIT_ONCE runs once (and again after a failure), a waiting thread stands still holding none of the locks; `tests/threads.rs` the same at scale on real threads | `formal/loom.sh` |
| SuspendThread / ResumeThread of running threads (`crates/waits/src/suspend.rs`, sake's `k32`): suspend counts, a suspended thread standing still at its run loop's next slice (kura::cpu), ResumeThread waking it, a parked thread leaving for a save state | loom (`tests/loom.rs` mod suspend): a thread suspended before its safe point runs nothing until resumed, concurrent calls count like one order, no lost wakeup (without the notify loom reports the deadlock); proptest: any sequence of calls gives Windows' counts (limit 127); `tests/winref/probes/suspend.c` against Windows | `formal/loom.sh` |
| koji's EFLAGS (`flags.rs`, the JIT's lazy flags `lazy.rs`) | Kani: ADD/ADC/SUB/SBB/CMP flags at 8/16/32/64 bits against the SDM's definitions on wide signed integers, Jcc conditions, logic flags, PDEP/PEXT against the SDM's pseudocode; the lazy flags (ADD, SUB, ADC, SBB, LOGIC, INC/DEC keeping CF, SHL/SHR/SAR with the SDM's CF and 1-bit OF) equal to the eager ones, for every operand, carry and count | `formal/kani.sh` |
| The store's FastCDC cutter (`ippai/src/cdc.rs` Cutter::next, its steps in Verus) | Verus (Z3), unbounded: for any bytes fed in any pieces, a cut is never before `min` bytes of the chunk nor past `max`, a chunk without a cut stays shorter than `max`, the cut is inside the bytes given (proptest `any_bytes_any_reads` checks the real code the same way, bounded) | `formal/verus.sh` |
| unsafe conversion loops on exactly sized buffers, movie slots across threads, padded GPU uploads | Miri | `formal/miri.sh` |
| Shader translation (HLSL → DXBC / D3D9 bytecode, D3D9 assembly, GLSL → WGSL): what the shaders compute on a real WebGPU (Chromium's Dawn) against the D3D11.3 functional spec, the D3D9 instruction reference, HLSL constant packing, GLSL 1.20 and std140 | `crates/shaderconf` writes the cases (shader, uniform data per pixel, expected pixel), `tests/shaders.mjs` draws them | `cargo run -q -p shaderconf > c.json && node tests/shaders.mjs c.json` |

`tla/models.txt` lists the configurations. Seven of them must fail, as records of known limits:

- `GpuQueueTimeouts.cfg`: when `wait_done` gives up (its 1 s deadline, or a page that has not
  polled for 3 s) the guest may change pixels the page is still uploading: a torn texture for a
  frame, never a memory error (guest memory stays mapped). The waits give up to avoid a hang.
- `VideoSlotsFlag.cfg`: `reading` as a flag (as it was) lets two guest threads reading the same
  slot free it under each other; `reading` is a count now.

- `SnapPauseNoFence.cfg`: `take` as it was: READY once every thread said PARKED; a thread says
  it holding GATE and lets go only in its wait, so one preempted in between held GATE in the copy
  (for ever in the restored module). The coordinator takes GATE once before READY now
  (`ready_waits_for_a_parked_thread_to_reach_its_wait`).
- `SnapPauseSpurious.cfg`, `SnapPauseGiveUp.cfg`: a spurious wake of a standing thread, or
  resume_all giving up its 2 s wait for the parked threads to wake, lets a thread wake while the
  page copies: detected (`Ctl::stirred`), the page drops that state.
- `RepeatSkipOld.cfg`: the rule before the model tests: a counterexample (below).
- `RepeatSkipUseful.cfg`: the rule does skip some batches (it is not vacuously safe).
- `MfEngineLifeOld.cfg`: the engine as it was (the block and its state stayed after the last Release): Freed is violated. `MfEngineLifeNoWake.cfg`: a notify on events only, no timeout: the thread asleep at the shutdown never ends.
- `StopCallsOld.cfg`: the splitter thread as it was (no enter, Stop not waiting): a SyncRead into krmovie's reader after the game freed it. `StopCallsGiveUp.cfg`: Stop's timeout (a call that never comes back) allows the same: the price of not hanging.

Found and fixed by the WebGPU model: `repeats()` skipped a batch that sampled a texture and then
wrote it from the blob, one that sampled a texture before clearing it, and one that cleared one
layer of an array texture and drew into another (clears were by texture); a pass begun before a
Siglus segment ended let later draws keep the segment's stamp (the next frame redrew only part of
a changed target); deferred clears did not mark the texture changed for draws sampling it;
Direct3D 11 depth buffers no draw had used were cleared to 1 by the optimized passes while plain
ones loaded WebGPU's zeros (they start at 1 now in both).

loom found: a reader of an SRW lock gave up when another reader's count landed between its
load and its exchange, and waited for a release (20 ms, or for ever where the readers holding it
wait for it: three readers that each hold the lock until all have it deadlocked); it tries again now.

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

The trace checks found: D3D9 state blocks did not hold what they captured (a texture released
after CreateStateBlock was destroyed, and Apply bound nothing where Windows binds it); the device
never released the IDirect3D9 it holds (its Release returned 1 at the end); releasing one device
tore down another made after it; a D3D11 swap chain kept its reference on the device after it was
destroyed (the device's count drifted up). Deliberately left as they are, and written into
ComRefs: resources do not hold their device, and Reset / ResizeBuffers do not fail while the
application still holds old buffers.

The lock, map, name and drawing-state traces found: D3D9 ColorFill succeeded on surfaces
outside D3DPOOL_DEFAULT; D3D11 Map took map types its resource does not allow (any type on
DEFAULT resources, WRITE_DISCARD on staging ones) and MAP_WRITE on a staging texture lost what
a GPU copy had put there (Unmap wrote stale memory back over it); GL deletes left the object
bound and attached, glIs* was true for names only generated, glGen* could hand back a name
already in use (by a bind of an ungenerated name, or a shader), attachments took objects that
did not exist, glGen*/glDelete* with n < 0 set no error, and glIsRenderbuffer was missing;
D2D BeginDraw/EndDraw kept no state (EndDraw always succeeded, drawing outside them drew).
Deliberately left: a deleted texture or renderbuffer is detached from every framebuffer, not
only the bound one (GlNames says so). The first ComRefs CI failures were the spec's: a level
of a texture bound as a render target did not keep the texture alive in the model.

The interpreter oracle found: D3D9 pixel shaders with texld, dsx or dsy inside if blocks or
loops were invalid WGSL for Chromium (its uniformity analysis was not turned off for D3D9 as it
is for DXBC; naga does not check it, so only the browser saw it); dxwgsl took invalid bytecode
and gave it a meaning the instructions do not have: DXBC destinations with no components
(written as all four), _sat on integer instructions, D3D9 destinations with write mask 0 and
parameter tokens without bit 31 (a missing destination read as none), D3D9 defs of non-constant
registers. And one decoding bug: dxwgsl read a DXBC instruction's length from 5 bits instead
of 7, so an instruction 41 dwords long was read as 9. They are rejected now
(`crates/shaderconf/regress/oracle7-*.txt`).

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
