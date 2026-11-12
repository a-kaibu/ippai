# Save states (どこでもセーブ)

A save state is the whole running program at one moment, independent of the game's own saves:
taken at any time from the button in the top right corner of the game, loaded later (also on
another device) to go on from exactly there.

## What a state is

The module's whole linear memory, taken while every thread stands still where a copy can go on
from. Everything outside the memory is made again from it:

| outside the memory | how it comes back |
| --- | --- |
| the threads' host stacks | every thread stands where nothing on its stack matters (below); a restored module starts a thread per record |
| the GPU (textures, buffers, shaders, pipelines) | the drawing thread reads it all back into one batch that makes it again (`crates/kura/src/render/d3d/snap.rs`); a 4x texture (which WebGPU can't copy) sample by sample: each drawn into a 1x texture of its format and read, restored by drawing each into its samples with a multisample mask |
| translated x86 code (JIT) | per thread, translated again (the shared cache is in memory) |
| clocks | an offset makes the program's time go on from the state's (`kura::wrap` clock_gettime) |
| the page (audio, movies, input) | the page is new; the guest's rings and queues are in memory |

### Where a thread stands still (`crates/sake/src/snap.rs`)

* **CONTINUE**: at the top level of the run loop, between blocks of guest code (kura's
  `Stop::Pause`). It goes on there.
* **RESTART**: waiting in an API called from the top level that may simply be called again
  (`RESTARTABLE`: Sleep, WaitFor*, critical sections, SRW locks, GetMessage, ...): its waits come
  before anything it does. A restored thread calls it again with the same stack.
* **RETURN**: in a condition variable's wait (a wake without a signal is allowed): the lock is
  taken again and the API returns as woken.
* **START**: created suspended, not started yet.

Host threads (the file fetcher, input, the sound mixer, the movie thread) say when they are idle
(`kura::snap::idle/busy`); work handed between threads (JIT compiles, region saves) is counted
(`DRAIN`) and waited for. While the page copies, nothing of the module runs: a lock held at that
moment would be held for ever in the restored copy. So every thread stands still in an untimed
wait (`stand_still`); an idle host thread's timed wait ends (woken) and it stands still in
`busy`, which the coordinator waits for before READY. A thread that still wakes then (a spurious
wake) is counted: the module says `Ctl.stirred` after RESUME and the page drops that state and
takes it again (`kura::snap` tests `nothing_runs_while_the_page_copies`, `a_wake_while_copied_is_told`,
checked against mutants of each of these rules).

The page writes into the memory too, outside any thread of the module: WebCodecs' decoded
frames (`VideoFrame.copyTo` straight into a slot, ending later) and samples (web/src/video.ts,
adec.ts), input, chunks read ahead. Before SAVE the page shuts `memGate` (web/src/memgate.ts):
nothing new is written, the copies under way end first (a write across awaits holds the gate),
and what comes while it is shut (a decoder's output, a key, a resize) is done after RESUME in the
order it came. tests/webcodecs-snap-browser.mjs: real decoders, the first frame copy made late,
the memory unchanged while shut and the frames and samples after it the same as a run without.

A program that started other processes (their kernel is shared outside the memory,
host/vnkern.js) can't be saved: "other processes run with this one".

A thread anywhere else (a modal dialog's loop, a window procedure waiting on another thread, a
16-bit program, an exception being handled) keeps the state from being taken: the save fails
after 5 seconds and says where it was (`still running: ... (user32!DialogBoxParamA)`).

### Taking one (`crates/kura/src/snap.rs`)

The page sets `Ctl.cmd = SAVE`. The coordinator thread asks every thread to stand still (and wakes
the ones that sleep), waits until all have, has the drawing thread read the GPU back
(`Ctl.gpu`), lists the memory to copy (the host's statics, heap and stacks, and the guest's
reservations) and says READY. The page copies the blocks and says RESUME; the time the program
stood still is taken off its clocks.

### Restoring

The page makes a memory, fills it from the state and sets `Ctl.restoring`, then instantiates the
module with that memory (emscripten skips the data segments: the image says they are done).
sake's main sees `restoring` and, instead of starting the program, sets the clocks' offsets,
restarts the host services and starts each guest thread from its record. The drawing thread
runs the GPU batch before the guest's batches.

## The file

`IPPAISNP`, u32 version (1), u32 header length, the header (JSON: `format`, `build`, `wasm`
(sake or sake-grow), `pages` (the memory's size: sake-grow's grows), `ctl`, `raw`, `created`),
then blocks: u32 address, u32 length, u32 compressed length and a zstd frame
of 1 MiB at most. Blocks of zeros are left out. An address of 0xffffffff ends it. A state is of one
build of sake (`build-id.txt`: each build's name and a hash of its .wasm): another build can't
load it. The page loads a state in the build it is of (a sake-grow state on a device that could
have 4 GB too); a sake.wasm state needs the 4 GB memory.

States of an older build load in that build's own web app (its sake and its page code, which
change together): `ippai serve` keeps each web app it served in `<store>/apps/<key>/` (key: a hash
of its build-id.txt) and serves it at `/apps/<key>/`, its own files from there and every other
path as at the root (saves, store, jit, the API), so the old app's relative URLs reach the same
server (crates/ippai/src/apps.rs). `/apps/builds` maps build ids to keys; the page opens a state
of another build at `/apps/<key>/play.html?...&state=<slot>`. Kept: the apps the states' indexes
name, and the 3 newest. Apps from before this (served by an ippai older than 2026-11-08) were
never kept. When a retained app navigates back to its library (`play.html` without `game` or
`run`, or `index.html`), the server redirects to the current app. The switches remain, but
`state` and `autostart` are removed. This works for already-retained page code too.
`build-id.txt` and `jit-version.txt` are not validated by modification time: extracted builds
or rollbacks can share a timestamp while having incompatible contents. Wasm and versioned
assets keep their normal caching.

`tests/apps-browser.mjs` checks retained routing, current-library navigation, and equal-mtime
build identity in real Chromium against a server upgrade. For real runtime restoration:

```sh
OLD_WEB=/path/to/old/web NEW_WEB=/path/to/new/web \
  node tests/savestate-browser.mjs target/release/ippai
```

Both directories must contain complete, distinct sake and sake-grow builds and their matching
page code. The test takes states in the old build, switches the same server/store to the new
build, opens the states from the new page on the original and a fresh browser profile, checks
the exact remaining guest output (including threads and clocks), checks requests for the
retained Wasm, and verifies return to the current library. It covers fixed memory, grown memory,
and Direct3D 11 MSAA sample readback, and prints retained apps' actual byte count. The page-tests
CI job selects a distinct prior successful main `web` artifact when one is still retained;
missing/expired fixtures are reported explicitly in the job summary.

Sizes: the test program (host/tests/snaptest.c) 42 MiB of memory → 2.5 MiB, standing still
0.15 s in Chromium (zstd level 1 in a worker, ~660 MB/s).

## Where states are kept

* this browser: OPFS `states/<game>/<hash>.snap` and `index.json` (4 slots);
* the server: `ippai serve` `/saves/state.<game>.<hash>` and `/saves/state.<game>.index` (the
  same versioned saves as the game's own, in its S3 bucket when one is set up): another device
  lists and loads them too.

A state file is named by its content (`hash`: 128 bits of SHA-256, in the index entry), written
once and never over; only the index changes, with If-Match, merged again with the server's when
another device wrote it meanwhile (the newer entry of each slot wins). So a slot saved from a PC
and a phone at once never leaves the index naming a file that was replaced
(tests/savestate-slots-browser.mjs, every order of the two uploads). States from before (no
`hash`) are read from `<slot>.snap` / `state.<game>.<slot>`. Files no index names stay on the
server (this browser removes its own).

A state holds the game's own save files as they were then: loading one goes back to those.

## Limits

* Depth and stencil buffers can't be read back (WebGPU): they come back as last cleared (games
  clear them every frame).
* Under node, files opened outside kura's file system (NODERAWFS) are not in a state.
* Node 22's V8 loses `i32.atomic.store` to constant addresses at 2 GiB and up in its first tier,
  where all of the host's statics are: run sake.js under node 26 (what CI uses).

## Under node, and the tests

host/snap.js (the node build's pre-js):

    IPPAI_SNAP_SAVE=<file>@<s>[,<s>...]   states at those seconds (file, file.1, ...)
    IPPAI_SNAP_EXIT=1                     end right at the (last) state, without going on
    IPPAI_SNAP_LOAD=<file>                start from a state

* `tests/snap.sh`: snaptest.c (events, critical sections, condition variables, SRW locks,
  Sleep, a thread started late, the clocks) for x86 and x86-64, JIT and interpreter: cut at five
  moments, the output before the cut and after the restore must be the whole run's; restored
  twice the same; a state of a restored run; live states change nothing; no run stirred; and
  host/tests/snapproc.c (another process): no state, the program goes on.
* `a_state_draws_the_same_d3d9/11` (crates/kura/src/render/d3d/model_tests.rs): random Direct3D
  command streams on the WebGPU model, with the GPU read back after a random batch and made again
  in a fresh model: every later frame and readback the same.
* `tests/savestate-browser.mjs`: in Chromium with ippai serve (also host/tests/snapmsaa.c:
  Direct3D 11 4x targets of 4 formats drawn once, every sample read back each step, the same
  bits after the state; it fails without the sample copies), a state from the dialog, loaded,
  and loaded in a browser with nothing of its own (the server's copy); again on sake-grow with
  its memory grown by 512 MiB before the state, loaded without `&memory=grow`.
