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

`tla/models.txt` lists the configurations. Two of them must fail, as records of known limits:

- `GpuQueueTimeouts.cfg`: when `wait_done` gives up (its 1 s deadline, or a page that has not
  polled for 3 s) the guest may change pixels the page is still uploading: a torn texture for a
  frame, never a memory error (guest memory stays mapped). The waits give up to avoid a hang.
- `VideoSlotsFlag.cfg`: `reading` as a flag (as it was) lets two guest threads reading the same
  slot free it under each other; `reading` is a count now.

Found and fixed with these: the movie slot flag above; `f16_bits` rounded ties up (4535 of every
7th f32 differed from IEEE round to nearest even); 5- and 6-bit channels were expanded by bit
replication, off by one from D3D's UNORM rule for 14 values.
