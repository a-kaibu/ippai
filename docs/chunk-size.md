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
