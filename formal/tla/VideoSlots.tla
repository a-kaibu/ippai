--------------------------- MODULE VideoSlots ---------------------------
(* Movie frames the page decodes with WebCodecs, handed to the guest through a stream's slots
   (crates/kura/src/video.rs). The page writes a decoded frame into a free slot between
   vnhost_video_begin (which may reallocate the slot's buffer) and vnhost_video_ready; guest
   threads read a ready slot in `frame`, converting its pixels outside the lock; `close` marks the
   stream closed and it is dropped (with its buffers) when no slot is written or read any more.

   Safety: no reader converts a buffer that was freed (NoUseAfterFree) or that the page is
   writing (NoTornRead); the page never writes a freed buffer.
   Liveness: a closed stream is dropped once its readers and the page are done.

   BoolReading = TRUE models `reading` as a flag instead of a count of readers: with two guest
   threads reading the same slot, the first one to finish clears it while the second still
   converts, and the page may reuse (and reallocate) the slot under it. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Readers, Slots, MaxBuf, BoolReading

VARIABLES
  exists, closed,
  writing,   \* per slot
  readers,   \* per slot: how many guests read it (BoolReading: 0 or 1)
  ready,     \* per slot: holds a decoded frame
  buf,       \* per slot: its buffer (an allocation number)
  live,      \* allocations not freed
  next,      \* next allocation number
  held,      \* per reader: <<slot, buffer>> it converts, or <<>> for none
  pw,        \* the page: slot it writes, 0 for none
  bad

vars == <<exists, closed, writing, readers, ready, buf, live, next, held, pw, bad>>

Init == /\ exists = TRUE /\ closed = FALSE
        /\ writing = [s \in Slots |-> FALSE] /\ readers = [s \in Slots |-> 0]
        /\ ready = [s \in Slots |-> FALSE]
        /\ buf = [s \in Slots |-> s] /\ live = Slots /\ next = Cardinality(Slots) + 1
        /\ held = [r \in Readers |-> <<>>] /\ pw = 0 /\ bad = FALSE

\* `release`: dropped when closed and no slot is written or read
Release(c, w, rd) ==
  IF exists /\ c /\ (\A s \in Slots : ~w[s] /\ rd[s] = 0)
    THEN exists' = FALSE /\ live' = {}
    ELSE UNCHANGED <<exists, live>>

\* ---- page ----

\* vnhost_video_begin: a slot nobody reads or writes; a bigger frame reallocates its buffer
Begin(s, grow) ==
  /\ exists /\ ~closed /\ pw = 0
  /\ ~writing[s] /\ readers[s] = 0
  /\ writing' = [writing EXCEPT ![s] = TRUE] /\ ready' = [ready EXCEPT ![s] = FALSE]
  /\ pw' = s
  /\ IF grow /\ next < MaxBuf
       THEN /\ buf' = [buf EXCEPT ![s] = next] /\ next' = next + 1
            /\ live' = (live \ {buf[s]}) \cup {next}
       ELSE UNCHANGED <<buf, next, live>>
  /\ UNCHANGED <<exists, closed, readers, held, bad>>

\* the page copies into the buffer, then vnhost_video_ready (ok or failed)
Ready(ok) ==
  /\ pw # 0
  /\ bad' = (bad \/ buf[pw] \notin live)
  /\ writing' = [writing EXCEPT ![pw] = FALSE]
  /\ ready' = [ready EXCEPT ![pw] = ok]
  /\ pw' = 0
  /\ Release(closed, writing', readers)
  /\ UNCHANGED <<closed, readers, buf, next, held>>

\* ---- guest threads ----

\* `frame`: takes a ready slot, then converts outside the lock
Take(r, s) ==
  /\ exists /\ ~closed /\ held[r] = <<>>
  /\ ready[s] /\ ~writing[s]
  /\ readers' = [readers EXCEPT ![s] = IF BoolReading THEN 1 ELSE @ + 1]
  /\ held' = [held EXCEPT ![r] = <<s, buf[s]>>]
  /\ UNCHANGED <<exists, closed, writing, ready, buf, live, next, pw, bad>>

Convert(r) ==
  /\ held[r] # <<>>
  /\ LET s == held[r][1] b == held[r][2] IN
       /\ bad' = (bad \/ b \notin live \/ writing[s] \/ buf[s] # b)
       /\ readers' = [readers EXCEPT ![s] = IF BoolReading THEN 0 ELSE @ - 1]
  /\ held' = [held EXCEPT ![r] = <<>>]
  /\ Release(closed, writing, readers')
  /\ UNCHANGED <<closed, writing, ready, buf, next, pw>>

Close ==
  /\ exists /\ ~closed
  /\ closed' = TRUE
  /\ Release(TRUE, writing, readers)
  /\ UNCHANGED <<writing, readers, ready, buf, next, held, pw, bad>>

Next == \/ \E s \in Slots, g \in BOOLEAN : Begin(s, g)
        \/ \E ok \in BOOLEAN : Ready(ok)
        \/ \E r \in Readers, s \in Slots : Take(r, s)
        \/ \E r \in Readers : Convert(r)
        \/ Close

Spec == Init /\ [][Next]_vars
        /\ WF_vars(\E ok \in BOOLEAN : Ready(ok)) /\ \A r \in Readers : WF_vars(Convert(r))

Safe == ~bad
Dropped == closed ~> ~exists
=============================================================================
