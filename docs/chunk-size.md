# Chunk size for local play (measured 2026-11-08)

The 577a146 review asked whether FastCDC's 16 MiB average (`pack.rs` DEFAULT_CHUNK_KIB, chosen
for S3 PUT counts: 1.9 GiB pushed in 69 s at 16 MiB, 105 s at 4 MiB, 358 s at 1 MiB) is also
right for playing from a local server and the device's cache. `tests/chunkbench/run.mjs`: a
192 MiB incompressible archive (as packed games' are) read by `chunkread.exe` as a VN reads
one (its head, 200 reads of 64 KiB at random places, 32 MiB sequential), from ippai serve on
the same machine, per size a first start (nothing on the device), a second (chunks in Cache
Storage) and one offline. kura's chunk cache was its default 128 MiB. Release sake.wasm, 4-core
container; "Android-like" = `tests/android-like.sh` (one busy loop per core, ~the user's phone
per bench.html) plus `THROTTLE=4` on the page's main thread. Times in ms; p50 was 0 everywhere
(sake's clock is 1 ms).

Android-like:

| avg chunk | run | start to first line | random p95 | random max | 200 random reads | sequential 32 MiB | peak RSS |
|---|---|---|---|---|---|---|---|
| 1 MiB | first | 3947 | 329 | 3649 | 17248 | 332 | 1735 MiB |
| 1 MiB | second | 1551 | 244 | 390 | 11063 | 305 | 2001 MiB |
| 1 MiB | offline | 1477 | 296 | 451 | 11952 | 408 | 1958 MiB |
| 4 MiB | first | 3524 | 285 | 556 | 8358 | 160 | 1452 MiB |
| 4 MiB | second | 1982 | 53 | 78 | 2673 | 198 | 1465 MiB |
| 4 MiB | offline | 2152 | 44 | 105 | 2620 | 76 | 1467 MiB |
| 16 MiB | first | 3706 | 217 | 2946 | 12847 | 574 | 1599 MiB |
| 16 MiB | second | 2082 | 187 | 510 | 7914 | 442 | 1869 MiB |
| 16 MiB | offline | 1361 | 210 | 440 | 8619 | 319 | 1825 MiB |

A second run with 2, 4 and 8 MiB (asked 2026-11-08):

| avg chunk | run | start to first line | random p95 | random max | 200 random reads | sequential 32 MiB | peak RSS |
|---|---|---|---|---|---|---|---|
| 2 MiB | first | 4658 | 256 | 432 | 8990 | 105 | 1343 MiB |
| 2 MiB | second | 1919 | 56 | 197 | 3048 | 146 | 1415 MiB |
| 2 MiB | offline | 1923 | 38 | 82 | 1926 | 155 | 1353 MiB |
| 4 MiB | first | 3130 | 256 | 1073 | 9404 | 423 | 1433 MiB |
| 4 MiB | second | 1623 | 59 | 119 | 2609 | 172 | 1519 MiB |
| 4 MiB | offline | 1207 | 50 | 84 | 2722 | 134 | 1558 MiB |
| 8 MiB | first | 2964 | 132 | 1657 | 11678 | 515 | 1491 MiB |
| 8 MiB | second | 1944 | 75 | 277 | 4229 | 207 | 1583 MiB |
| 8 MiB | offline | 2244 | 126 | 128 | 4179 | 426 | 1585 MiB |

2 and 4 MiB are within run-to-run noise of each other (4 MiB's second run: 2673 then 2609 ms);
8 MiB is ~1.5x slower on the device's copy. 4 MiB is the default: as fast as 2 MiB with half
the chunks (requests, S3 objects, Cache Storage entries, tree lines).

Unthrottled the order is the same (second run, 200 random reads: 1 MiB 5499, 4 MiB 1683,
16 MiB 4708 ms).

So for scattered reads 4 MiB is clearly best once the chunks are on the device (3x fewer ms
than 16 MiB: a 64 KiB read that misses kura's 128 MiB cache costs a whole chunk's read from
Cache Storage and zstd decode, and only 8 chunks of 16 MiB fit), and also lowest in memory.
1 MiB being slower than 4 MiB was not expected (more per-chunk requests through the network
worker; issue filed). Starts and sequential reads differ less.

JIT (`tests/chunkbench/jit.mjs`, flatbench.exe: Lua + SQLite, rounds of work), Android-like,
6 rounds: first run round 1 12226 ms (everything translated as it runs), steady 4633 ms; with
the regions saved by the run before: round 1 8110 ms, after one more run 4484 ms (as steady).
Unthrottled: 3259 -> 2610 -> 2412 ms for round 1, steady ~1700-1900 ms. Saved regions remove
most of the first-run penalty from the second start on; regions are only saved after a program
ran 20 s (kura jit.rs maybe_save), so a short first run saves nothing.

## After blocks and ranges (2026-11-08, second measurement)

The numbers above measured the implementation, not the size: a 64 KiB read that missed kura's
cache read its whole chunk out of Cache Storage (`Response.arrayBuffer()`: ~1.4 ms per MiB in
Chromium here), copied it into the wasm heap and again into a `Vec`, and kept it as one cache
entry (8 entries of 16 MiB in 128 MiB); over the network it fetched the whole chunk, plus the
next whole chunk ahead. So big chunks lost on scattered reads and 1 MiB lost on requests.

Now (crates/kura/src/vfs.rs, host/fetch-bridge.js, crates/ippai/src/store_api.rs):

- kura caches and asks for **blocks** of 256 KiB (`IPPAI_VFS_BLOCK_KIB`), fetched in pieces of up
  to 1 MiB: a read asks for its own blocks, the fetcher threads for the rest of it and one block
  past it (32 for a sequential read).
- The network worker answers a block range from the first place that has it: its memory (a chunk
  it fetched whole lately), the whole chunk in Cache Storage with `Blob.slice` (~1.3 ms for 64 or
  256 KiB of an entry of 1, 4, 16 or 64 MiB alike), the chunk's blocks kept alone in Cache Storage
  (`<chunk>?b=<n>&s=<block>`), else a `Range` request.
- `ippai serve` answers `Range` on a chunk with those bytes of the decoded chunk (206, no
  Content-Encoding), decoding each chunk once (256 MiB of decoded chunks kept, one decode for
  requests side by side). A server without ranges sends the whole chunk: then checked and kept
  whole as before. A block has no name of its own to check: its place and length are checked, the
  BLAKE3 check stays for whole chunks (the page's, 端末に保存).

Same machine and harness as above (Android-like, `THROTTLE=4`, deterministic data.bin: every
size's runs read the same bytes and the program's sums matched the old build's at every size).
`NET=20000,40`: a first start over a 20 Mbit/s, 40 ms link, 50 scattered reads of 128 KiB
(`NET_READS`), in a profile of its own. Old build = 55b76c0. ms, two runs each for the new build:

| avg chunk | old: second, 200 random reads | new: second | old: 20 Mbit first, total | new: 20 Mbit first, total | new: fetched |
|---|---|---|---|---|---|
| 1 MiB | 1100 | 1133 / 1036 | 54581 | 29926 / 29675 | 64 MiB |
| 2 MiB | 2053 | 860 / 1049 | 73482 | 29934 / 30035 | 65 MiB |
| 4 MiB | 2781 | 1035 / 1162 | 84627 | 30499 / 30488 | 66 MiB |
| 8 MiB | 4970 | 912 / 999 | 84299 | 30343 / 30298 | 66 MiB |
| 16 MiB | 7845 | 794 / 1018 | 87691 | 30041 / 30202 | 64 MiB |
| 32 MiB | 16602 | 894 / 818 | 92507 | 30194 / 30430 | 65 MiB |
| 64 MiB | - | 1024 / 1029 | - | 31426 / 31887 | 65 MiB |

(the old build fetched 117-192 MiB over the link: whole chunks and the next one.) A first start
from a server on the same machine (no link limit): 200 random reads 4009 ms at 4 MiB, 4522 at
16, 4727 at 32, 4280 at 64 (old: 7496 / 12221 / 19452 / -); the slowest single read grows with
the size (70 / 147 / 248 / 359 ms): the server decodes a chunk whole the first time any of it is
asked for. A real game (the KiriKiri bench game, 4 and 16 MiB) reached its title in the same
time with both builds (5 runs each: 2.7-3.8 s).

So for playing, the chunk size no longer matters from 1 to 64 MiB. What still depends on it:

- the server's first touch of a chunk: one S3 GET of the whole object, its decryption and its
  decoding before the first block goes out (above: +80 ms from 4 to 16 MiB on the same machine;
  from S3, plus the object's transfer), and 256 MiB of decoded chunks hold 64 of 4 MiB, 16 of 16;
- pushing to S3: 1.9 GiB took 358 s at 1 MiB, 105 s at 4 MiB, 69 s at 16 MiB (above); the
  166 titles (763 GB) are ~190 000 objects at 4 MiB, ~48 000 at 16 MiB;
- the page's read-ahead and 端末に保存 move whole chunks (its 64 MiB budget is 16 chunks of
  4 MiB, 4 of 16);
- deduplication: smaller chunks share more (cdc.rs; not measured per size here).

4 MiB stays the default: play is as fast as with any other size, the first touch and the server's
memory favour it over bigger ones, and only pushing is slower than at 16 MiB.
