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

  A 429 empties the budget for a while, for everyone.
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
  pictures; scripts. A read hints the next files of the folder of the same kind: 12 sounds,
  6 pictures, 16 scripts, 4 of an unknown kind (same extension). Each must be at most 8 MiB and
  all of them at most 24 MiB.
- **Archives** (one big file, its index then its members): members packed side by side are read
  side by side, so a read hints the next chunks: 4 for KiriKiri `.xp3`, 6 for Unity, Godot and
  Ren'Py, 3 for Artemis, NScripter, CatSystem2 and the like, 1 for Siglus `.ovk` (one
  character's voices, read by line number, so scattered), 2 for the rest. An archive of at most
  16 MiB (a script pack read whole at the start) is hinted whole at its first read.

Movies are never hinted: they are read front to back as they play (vfs.rs `SEQ_AHEAD`).

## Still to do

- Delivery packs: the existing chunk ciphertexts concatenated per game in read order, read with
  Range, so that one request carries many chunks (storage format unchanged).
- A first-play read list from prerun, so a game nobody has played yet gets a warm too.
- OPFS sync reads for a saved game, from the pthreads.
