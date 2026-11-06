--------------------------- MODULE GpuReadback ---------------------------
(* Readbacks (GetRenderTargetData, Map of a staging texture, glReadPixels): the guest records
   READBACK, then waits in wait_readback (crates/kura/src/d3d9/gpu.rs) until the page wrote the
   pixels into guest memory (`dst`) and called vnhost_d3d9_readback_done; it gives up when the
   page stalls (10 s without a finished batch) or stops polling, and the caller may then free
   `dst` at once. The page (Render::mapped in crates/kura/src/render/d3d.rs) asks
   vnhost_d3d9_readback_begin before it copies: 0 once the waiter gave up.

   Safety (NoWriteAfterFree): the page never writes into `dst` after its waiter returned.
   Liveness: every waiter returns; NoLeak: nothing of a finished request stays in the lists. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Req   \* readback ids (one guest thread each)

VARIABLES
  waiter,    \* per id: "idle" | "wait" | "ok" (returned true) | "gaveup" (returned false, dst freed)
  page,      \* per id: "none" | "queued" (READBACK recorded) | "mapping" | "begun" | "copied" | "fin"
  doneL,     \* readbacks_done
  writing,   \* readbacks_writing
  dropped,   \* readbacks_dropped
  bad        \* the page wrote dst after the waiter returned false

vars == <<waiter, page, doneL, writing, dropped, bad>>

Init == /\ waiter = [r \in Req |-> "idle"] /\ page = [r \in Req |-> "none"]
        /\ doneL = {} /\ writing = {} /\ dropped = {} /\ bad = FALSE

\* guest: records READBACK, flushes, waits
Send(r) == /\ waiter[r] = "idle"
           /\ waiter' = [waiter EXCEPT ![r] = "wait"] /\ page' = [page EXCEPT ![r] = "queued"]
           /\ UNCHANGED <<doneL, writing, dropped, bad>>

Found(r) == /\ waiter[r] = "wait" /\ r \in doneL
            /\ doneL' = doneL \ {r} /\ waiter' = [waiter EXCEPT ![r] = "ok"]
            /\ UNCHANGED <<page, writing, dropped, bad>>

\* (the deadline or a page that stopped polling: any time, as far as the guest knows)
GiveUp(r) == /\ waiter[r] = "wait" /\ r \notin doneL /\ r \notin writing
             /\ dropped' = dropped \cup {r} /\ waiter' = [waiter EXCEPT ![r] = "gaveup"]
             /\ UNCHANGED <<page, doneL, writing, bad>>

ReadbackDone(r) == /\ writing' = writing \ {r}
                   /\ IF r \in dropped THEN dropped' = dropped \ {r} /\ UNCHANGED doneL
                                       ELSE doneL' = doneL \cup {r} /\ UNCHANGED dropped
                   /\ UNCHANGED <<waiter, bad>>

\* page: Render::readback; a texture it has not (or a depth one): readback_done at once
RunMissing(r) == /\ page[r] = "queued" /\ page' = [page EXCEPT ![r] = "fin"]
                 /\ ReadbackDone(r)
RunCopy(r) == /\ page[r] = "queued" /\ page' = [page EXCEPT ![r] = "mapping"]
              /\ UNCHANGED <<waiter, doneL, writing, dropped, bad>>

\* Render::mapped: ok and readback_begin != 0 -> copy rows; then readback_done either way
Mapped(r) == /\ page[r] = "mapping"
             /\ \/ /\ r \notin dropped   \* begin -> 1
                   /\ writing' = writing \cup {r} /\ page' = [page EXCEPT ![r] = "begun"]
                   /\ UNCHANGED <<waiter, doneL, dropped, bad>>
                \/ /\ page' = [page EXCEPT ![r] = "fin"]   \* map failed, or begin -> 0
                   /\ ReadbackDone(r)
Copy(r) == /\ page[r] = "begun"
           /\ bad' = (bad \/ waiter[r] = "gaveup")
           /\ page' = [page EXCEPT ![r] = "copied"]
           /\ UNCHANGED <<waiter, doneL, writing, dropped>>
Finish(r) == /\ page[r] = "copied" /\ page' = [page EXCEPT ![r] = "fin"] /\ ReadbackDone(r)

Guest(r) == Send(r) \/ Found(r) \/ GiveUp(r)
Page(r) == RunMissing(r) \/ RunCopy(r) \/ Mapped(r) \/ Copy(r) \/ Finish(r)
Next == \E r \in Req : Guest(r) \/ Page(r)

Spec == Init /\ [][Next]_vars /\ \A r \in Req : WF_vars(Page(r)) /\ WF_vars(Found(r))

NoWriteAfterFree == ~bad
Returns == \A r \in Req : (waiter[r] = "wait") ~> (waiter[r] \in {"ok", "gaveup"})
NoLeak == (\A r \in Req : waiter[r] \in {"ok", "gaveup"} /\ page[r] = "fin")
            => (doneL = {} /\ writing = {} /\ dropped = {})
=============================================================================
