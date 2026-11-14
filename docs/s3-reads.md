# Reads from the bucket

Games are stored in Hugging Face Storage Buckets (S3 API) as encrypted FastCDC chunks of 4 MiB
on average (docs/chunk-size.md). The browser fetches them straight from the bucket with signed
URLs (ippai serve `/store-direct`) and decrypts them itself. This note says why play from the
bucket felt slow, and what the browser does about it.

## What the bucket costs

Measured from Japan, 2026-11-13:

- Every object takes 0.4-2 s before its first byte, whatever its size. The origin is in the US,
  and the CDN in front of it answers Miss every time, so nothing is cached near the player.
- The bucket lets one address make 3000 requests in 5 minutes
  (`ratelimit-policy: "resolvers";q=3000;w=300`), which is 10 a second. Past that it answers 429.
- A game reads one file after another, synchronously. A scene that reads ten voices or pictures
  the device does not have waits ten times the latency above.

Throughput is fine once a request is going. Latency and the request count are the cost, so the
work is to ask early, to ask once, and to keep the request count under the limit.

## One scheduler (host/fetch-bridge.js)

Every request to the bucket goes through the network worker:

- **Fetch once.** The worker keeps one promise per chunk in flight (`ippaiInflight`). A second
  reader of the same chunk joins it, and a game read that joins a lower class's fetch raises that
  fetch to the game's class. Contexts outside the worker (the page's save to the device) take a
  Web Lock `ippai-chunk:<hex>` and look in the Cache first.
- **One budget.** A token bucket in a SharedArrayBuffer holds the request budget: it refills at
  `RATE` (9) a second up to `BURST` (40). The page and the worker share the same buffer
  (web/src/budget.ts has the page's copy, tested against the worker's). Classes, most urgent
  first:

  | class | what | leaves to those above |
  |-------|------|------------------------|
  | game  | a read the game waits on | may go 45 into debt |
  | ahead | a fetcher thread's read-ahead (`!a` on the range) | 0 |
  | hint  | what a read says comes next (below) | 8 |
  | warm  | the read-order list, fetched while the game plays | 16 |
  | save  | the whole game saved to the device | 16 |

  Every HTTP try takes from it, a retry too. A 429 stops every class, the game's too, for the
  wait the bucket asks (5 s when it names none), and the budget refills from none after it.
- **Who waits.** A pack's GET and the chunks it brings share one job: a game read of any of them
  makes the GET the game's. A game read that joins a fetch ahead still waiting for the chunk's
  Web Lock (held by the page's save) waits 1 s for it, then fetches the chunk itself.
- **Held until written.** A chunk checked but not yet on the device (its write to OPFS still
  going) is answered from the worker's memory, not fetched again; 64 MiB of them at most before
  the fetches past the game's wait. The page's save holds 128 MiB of pack runs at once (64 MiB
  on a phone of 4 GB or less), and leaves out of a run the chunks whose lock the worker holds.
- **Batch presign.** `POST /store-direct` with a JSON list of chunk paths answers each as
  `GET /store-direct/<path>` would, up to 4096 at once. The warm and the save ask for 256 at a
  time ahead of fetching them, so most bucket GETs need no round trip to ippai serve first.
- **Yield to the game.** While the game waits for a read, the save to the device keeps only 2
  fetches going, and the warm narrows to its busy width.

## Engine-aware hints (crates/kura/src/vfs/hints.rs)

When a read misses the device, kura says what the game will read soon, and the worker fetches it
in the background (`vnvm_fetch_hint` → `ippaiFetchHint`). How engines lay out their files
(vnrun's notes for each engine) gives two shapes:

- **Loose files in a folder**, read in name order: voices `v_0001.ogg`, `v_0002.ogg`, ...; CG
  pictures; scripts. A read hints the next files of the folder of the same kind: 48 sounds,
  24 pictures, 64 scripts, 16 of an unknown kind (same extension: .dll, .sig, fonts at start-up).
  Each must be at most 8 MiB and all of them at most 24 MiB.
- **Archives** (one big file, its index then its members): members packed side by side are read
  side by side, so a read hints the next chunks: 4 for KiriKiri `.xp3`, 6 for Unity, Godot and
  Ren'Py, 3 for Artemis, NScripter, CatSystem2 and the like, 1 for Siglus `.ovk` (one
  character's voices, read by line number, so scattered), 2 for the rest. An archive of at most
  16 MiB (a script pack read whole at the start) is hinted whole at its first read.

Movies are never hinted: they are read front to back as they play (vfs.rs `SEQ_AHEAD`).

## Delivery packs (crates/ippai/src/storage.rs pack_objects)

A pack is a game's chunk objects, exactly as the bucket has them (still encrypted, each with
its own key), side by side in one big object, in the order the game reads them. The chunk
objects themselves stay, so the storage format does not change and nothing reads them
differently; a pack is only a quicker way to the same bytes.

- `ippai storage packs [game...]` makes them, about 64 MiB each (`--mib`). The order is the
  game's read-order list first (`<jit>/game.<id>.chunks`: what earlier runs and serve's
  pre-run read first), then the rest of its trees in tree order. The prefix's chunks are left
  out (every game shares them). Running it again packs only what was added or replaced.
- The `packs` table names where each object is: pack, offset, length, and which object it was
  (an object replaced since is not answered from an old pack). A pack no row names any more is
  retired, and `storage gc` deletes it.
- `/store-direct` answers add `pack: {id, url, offset, len}`. In the network worker a read of a
  chunk takes the chunks after it in the pack with the same ranged GET, those not on the device
  nor coming already: up to 4 MiB for the game's own read (it waits for all of it), 8 MiB for
  read-ahead, 16 MiB for hints, 32 MiB for the warm and for a save. A read of one of them
  meanwhile joins the GET. The page's save takes whole runs (up to 32 MiB or 64 chunks), 16 runs
  at once.
- Each chunk from a pack is opened and checked against its name, as one fetched alone is. One
  that fails is fetched alone; a pack that fails sends all of them alone.
- Hugging Face's CDN answers a ranged GET with 206 and exposes `Content-Range` to every origin
  (checked 2026-11-13, through the 302 from huggingface.co).

- Each pack is marked as the game's it was made for (table `pack_games`), so what it costs can
  be seen and dropped game by game. The chunk objects stay either way: dropping a pack only
  makes its chunks read alone again.

```sh
ippai storage usage                 # by kind (chunks, packs, saves, ...), then each game:
                                    # its own chunks (freed if it goes), shared ones, its packs
ippai storage unpack saga_planets-2 # drop a game's packs ("?": packs from before marking)
ippai storage unpack --all
ippai storage gc --grace-days 0     # then delete them from the bucket
```

worker2's uplink is slow (about 100 KB/s, measured 2026-11-14: a 64 MiB pack did not get up in
10 minutes, and the upload slowed the players), so packs are made on a machine with a faster
line and merged in: `storage merge` takes another store's pack spans for the objects both
stores name, and retires the packs no row names.

```sh
# on the server: a backup of its database
ippai storage backup                                   # prints <uuid>
# on the PC (same storage config; the server's read-order lists copied to <jit>)
ippai --store <dir> storage merge <uuid>               # the server's objects, same UUIDs
ippai --store <dir> storage packs --jit <jit>
ippai --store <dir> storage backup                     # prints <uuid2>
# on the server
ippai storage merge <uuid2>
```

The user uploads the packs (this needs the bucket's write credentials):

```sh
IPPAI_STORAGE_CONFIG=... ippai storage packs            # every game
IPPAI_STORAGE_CONFIG=... ippai storage packs saga_planets-2
```

## Chunks on the device: OPFS (web/src/chunkstore.ts)

Chunks fetched or saved to the device are kept in the origin private file system, in the folder
`ippai-chunks`, one file each, named by the chunk's hex name (a block kept alone:
`<hex>?b=<k>&s=<block>`). Cache Storage (`ippai-chunks-v1`) was used before. It is still read,
so games saved before stay saved, and an entry there goes when it is kept or deleted again.

- The network worker reads a block of a whole chunk through a read-only synchronous access
  handle, kept open 5 s for the next blocks (32 at most). Measured in headless Chromium
  (tests/opfs-read-browser.mjs): a 256 KiB block of a 4 MiB chunk takes 1.4 ms against Cache
  Storage's 1.6 ms. At that size the disk dominates; the gain is bigger for small reads. It also
  avoids Cache Storage, which sometimes did not answer at all (IPPAI_CACHE_MS).
- A file exists empty from its creation, and its bytes land whole when it is closed. An empty
  file reads as not there.
- A file the worker holds open cannot be deleted. A delete from the page then leaves it, and
  the next trim takes it.

## How well the hints foresee real reads (crates/kura/src/vfs/hints_eval.rs)

`hints_eval` replays the read-order lists in the jit folder (`game.<id>.chunks`, else the
program's `sake-<prog>-<build>.chunks`) against the games' trees, for the hints and variants of
them, per engine (guessed from the archives a game packs):

```sh
HINTS_STORE=~/.local/share/ippai/store HINTS_JIT=~/.cache/ippai/jit \
  cargo test -p kura --lib hints_eval -- --ignored --nocapture
```

On worker2 (2026-11-14, 9 games with a list, 392 first reads), reads that waited for the network:

| variant | misses | hinted for nothing |
|---------|--------|--------------------|
| no hints | 100% | 0 |
| the first counts (12/6/16/4 files) | 49.2% | 608 MiB |
| no chunk hints | 59.2% | 86 MiB |
| 2x the chunks | 47.2% | 1063 MiB |
| whole archive up to 64 MiB | 49.0% | 631 MiB |
| 4x the files (now) | 36.5% | 622 MiB |

So the file counts went up 4x (SIBLING_BYTES keeps the bytes about the same); the chunk counts
and the whole-archive size stay, since more of them costs far more than it saves. The misses
left are mostly archives' first chunks, pictures, and start-up .dll/.exe/.sig files, which the
read-order list (the warm) fetches before the game asks.

## sake's wasm from the bucket (web/src/wasmsource.ts)

sake.wasm is ~23 MB (~5 MB as zstd). From worker2 it came at ~100 KB/s at times (2026-11-14:
anemoi sat on "起動中" for minutes, the first instruction not run yet). `ippai serve` with S3
puts each `<web>/sake/*.wasm` in the bucket once per build as a chunk (named by its BLAKE3,
zstd 19, encrypted: store_api.rs `publish_wasm`), and `GET /sake-direct/<file>` says how to
fetch it (a presigned URL, key, object, `chunk`), the server's own disk cache ignored. The page
(play.ts, before sake.js starts) takes, in order: the browser's HTTP cache of the server's
`?v=` file (`only-if-cached`: emscripten then loads it itself and V8 keeps its compiled code),
the device's chunk store (OPFS, from an earlier start), the bucket (then kept on the device),
else the server's file as before (an older server, no S3, a failing bucket). Bytes go to
sake.js through `Module.instantiateWasm`, compiled once per page (children share the module).
`&wasm=server` forces the server's file. Tests: store_api `sake_wasm_from_the_bucket`,
web/src/wasmsource.test.ts, tests/wasmsource-browser.mjs (Chromium, CI).

## Still to do

- Run hints_eval again when more games have read-order lists, per engine.
