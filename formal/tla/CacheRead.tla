---------------------------- MODULE CacheRead ----------------------------
(* A read of a chunk's bytes in the network worker (host/fetch-bridge.js masuFetchPart), and
   the game's read that waits behind it (kura's fetcher thread fetches a chunk's blocks ahead,
   "!a"; the game's read of those blocks waits for that fetch: crates/kura/src/vfs.rs).

   The reader looks in Cache Storage step by step: open it, match the chunk, then read its
   blocks one after another (kept alone, NBlocks of them; a short block is the chunk's last).
   Cache Storage may answer a step, fail it, or never answer it at all (Chromium left a cached
   body's read pending for good under load, 2026-11-13). A step it fails, or one past
   MASU_CACHE_MS (`Timeout`: masuCacheStep), is a miss: the bytes come from the network,
   which answers in the end (its own retries, masuFetchRobust).

   Correct: a read that ends has every block (the bytes asked for). Ends: the game's read ends.
   Timeout = FALSE is the reader as it was, each step awaited for good: one step that is never
   answered leaves the game waiting for good (Ends breaks). ShortBug = TRUE is the block loop
   as it was: a block whose body failed to read was kept as an empty part, which reads as the
   chunk's short last block, and the read ended short (Correct breaks). *)
EXTENDS Naturals

CONSTANTS NBlocks, Timeout, ShortBug

VARIABLES
  pc,    \* the reader: "open", "match", "block", "net", "done"
  step,  \* the Cache Storage step it waits for: "wait" (may still answer), "hung" (never will)
  got,   \* blocks it has
  game   \* the game's read: "join" (waits for the reader), "done"

vars == <<pc, step, got, game>>

Init == pc = "open" /\ step = "wait" /\ got = 0 /\ game = "join"

Waiting == pc \in {"open", "match", "block"}

\* Cache Storage answers the step
Answer ==
  /\ Waiting /\ step = "wait"
  /\ CASE pc = "open" -> pc' = "match" /\ got' = got
       [] pc = "match" -> pc' = "block" /\ got' = got
       [] pc = "block" -> got' = got + 1 /\ pc' = IF got + 1 = NBlocks THEN "done" ELSE "block"
  /\ UNCHANGED <<step, game>>

\* a miss: the network then; a block that did not read, under ShortBug, the end of the chunk
Miss(short) ==
  /\ IF short /\ pc = "block" THEN pc' = "done" ELSE pc' = "net"
  /\ step' = "wait"
  /\ UNCHANGED <<got, game>>

\* Cache Storage fails the step
Fail == Waiting /\ step = "wait" /\ Miss(ShortBug)

\* Cache Storage will never answer the step
Stall == Waiting /\ step = "wait" /\ step' = "hung" /\ UNCHANGED <<pc, got, game>>

\* the step took MASU_CACHE_MS
Tick == Timeout /\ Waiting /\ Miss(FALSE)

\* the network's answer: the whole range
Net == pc = "net" /\ pc' = "done" /\ got' = NBlocks /\ UNCHANGED <<step, game>>

\* the game's read gets what the reader got
Game == game = "join" /\ pc = "done" /\ game' = "done" /\ UNCHANGED <<pc, step, got>>

Next == Answer \/ Fail \/ Stall \/ Tick \/ Net \/ Game

\* (Cache Storage owes nothing: no fairness for Answer or Fail)
Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Net) /\ WF_vars(Game)

TypeOK == pc \in {"open", "match", "block", "net", "done"} /\ step \in {"wait", "hung"} /\ got \in 0..NBlocks /\ game \in {"join", "done"}
Correct == pc = "done" => got = NBlocks
Ends == <>(game = "done")
=============================================================================
