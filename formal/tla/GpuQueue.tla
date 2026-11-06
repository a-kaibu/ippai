---------------------------- MODULE GpuQueue ----------------------------
(* The command queue between guest threads and the render thread
   (crates/kura/src/d3d9/gpu.rs, the page side in crates/kura/src/render/d3d.rs), shared by
   Direct3D 8/9/10/11 and OpenGL, for one surface whose pixels the page uploads straight from
   guest memory (WRITE_TEXTURE_MEM, device.rs upload_keep):

   guest: Upload records a WRITE_TEXTURE_MEM into the open batch and remembers its generation
          (`pending`); Lock (settle) first makes the batches the page has not taken copy the
          rows (detach_mem), and waits for the batch the page runs right now when that one
          reads them (wait_done); Present / flush close the open batch; Destroy frees the memory
          at once, or later (deferred_free) while a batch that reads it is not done.
   page:  poll takes the next closed batch, runs its commands one by one (reading guest memory
          outside the lock), then vnhost_d3d9_done.

   Safety (NoTornUpload): every upload the page runs sees the pixels the guest had when it
   recorded it. With Timeouts the guest's waits may give up (wait_done's 1 s deadline, or a page
   that has not polled for 3 s): TLC then shows how a torn upload happens. *)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS MaxGen,    \* generations explored
          MaxVer,    \* writes of the surface's memory explored
          Timeouts   \* whether waits may give up

None == -1   \* (an Int that is no generation)

VARIABLES
  gen,       \* generation of the open batch
  open,      \* its commands
  closed,    \* closed batches not taken yet: <<[gen, cmds]>>
  reading,   \* the batch the page took last ([gen |-> None] for none)
  pc,        \* the page: next command of `reading` it runs (0: idle)
  done,      \* batches below this generation are done
  mem,       \* version of the pixels in guest memory
  pending,   \* the surface's pending upload generation (None: none)
  deferred,  \* generation the deferred free waits for (None: none)
  guest,     \* "idle" | "wait" (in wait_done) | "write" (may write the locked memory)
  waitgen,   \* what the guest waits for
  torn       \* the page ran an upload whose pixels had changed

vars == <<gen, open, closed, reading, pc, done, mem, pending, deferred, guest, waitgen, torn>>

\* a command: [k |-> "mem", v |-> version recorded] (reads guest memory when run) or
\*            [k |-> "copy", v |-> version copied into the blob]
Cmd == [k : {"mem", "copy"}, v : 0..MaxVer]
Batch == [gen : Int, cmds : Seq(Cmd)]

Detach(cs) == [i \in DOMAIN cs |-> IF cs[i].k = "mem" THEN [k |-> "copy", v |-> mem] ELSE cs[i]]
Reads(b) == \E i \in DOMAIN b.cmds : b.cmds[i].k = "mem"
IsDone(g) == done > g

Init ==
  /\ gen = 0 /\ open = <<>> /\ closed = <<>>
  /\ reading = [gen |-> None, cmds |-> <<>>] /\ pc = 0
  /\ done = 0 /\ mem = 0 /\ pending = None /\ deferred = None
  /\ guest = "idle" /\ waitgen = None /\ torn = FALSE

Close == /\ closed' = Append(closed, [gen |-> gen, cmds |-> open])
         /\ open' = <<>> /\ gen' = gen + 1

\* ---- guest ----

\* upload_keep: already in this batch -> nothing (the page reads the newest pixels)
Upload ==
  /\ guest = "idle" /\ deferred = None
  /\ pending # gen
  /\ open' = Append(open, [k |-> "mem", v |-> mem])
  /\ pending' = gen
  /\ UNCHANGED <<gen, closed, reading, pc, done, mem, deferred, guest, waitgen, torn>>

\* Present / flush: closes the open batch if it has anything
Flush ==
  /\ open # <<>> /\ gen < MaxGen
  /\ Close
  /\ UNCHANGED <<reading, pc, done, mem, pending, deferred, guest, waitgen, torn>>

\* lock_surface -> settle -> detach_mem (+ wait_done)
Lock ==
  /\ guest = "idle" /\ deferred = None
  /\ pending' = None
  /\ IF pending # None /\ ~IsDone(pending)
       THEN /\ closed' = [i \in DOMAIN closed |-> [closed[i] EXCEPT !.cmds = Detach(@)]]
            /\ open' = Detach(open)
            /\ IF reading.gen # None /\ reading.gen >= done /\ Reads(reading)
                 THEN guest' = "wait" /\ waitgen' = reading.gen
                 ELSE guest' = "write" /\ waitgen' = None
       ELSE /\ UNCHANGED <<closed, open, waitgen>>
            /\ guest' = "write"
  /\ UNCHANGED <<gen, reading, pc, done, mem, deferred, torn>>

WaitDone ==
  /\ guest = "wait"
  /\ IsDone(waitgen) \/ Timeouts
  /\ guest' = "write" /\ waitgen' = None
  /\ UNCHANGED <<gen, open, closed, reading, pc, done, mem, pending, deferred, torn>>

\* writes the locked memory, then Unlock
Write ==
  /\ guest = "write" /\ mem < MaxVer
  /\ mem' = mem + 1 /\ guest' = "idle"
  /\ UNCHANGED <<gen, open, closed, reading, pc, done, pending, deferred, waitgen, torn>>

\* Release of the surface: its memory is freed (and may be reused by the next one: a new
\* version) now, or once the batch of its pending upload is done
Destroy ==
  /\ guest = "idle" /\ deferred = None /\ mem < MaxVer
  /\ IF pending # None /\ ~IsDone(pending)
       THEN deferred' = pending /\ UNCHANGED mem
       ELSE mem' = mem + 1 /\ UNCHANGED deferred
  /\ pending' = None
  /\ UNCHANGED <<gen, open, closed, reading, pc, done, guest, waitgen, torn>>

\* the next device call frees what deferred_free holds once it is done
FreeDeferred ==
  /\ deferred # None /\ (IsDone(deferred) \/ Timeouts)
  /\ mem' = mem + 1 /\ deferred' = None
  /\ UNCHANGED <<gen, open, closed, reading, pc, done, pending, guest, waitgen, torn>>

\* ---- page (render thread) ----

Poll ==
  /\ pc = 0
  /\ IF closed = <<>>
       THEN reading' = [gen |-> None, cmds |-> <<>>] /\ UNCHANGED <<closed, pc>>
       ELSE /\ reading' = Head(closed) /\ closed' = Tail(closed) /\ pc' = 1
  /\ UNCHANGED <<gen, open, done, mem, pending, deferred, guest, waitgen, torn>>

Run ==
  /\ pc > 0 /\ pc <= Len(reading.cmds)
  /\ LET c == reading.cmds[pc] IN
       torn' = (torn \/ (c.k = "mem" /\ c.v # mem))
  /\ pc' = pc + 1
  /\ UNCHANGED <<gen, open, closed, reading, done, mem, pending, deferred, guest, waitgen>>

Done ==
  /\ pc > 0 /\ pc > Len(reading.cmds)
  /\ done' = IF reading.gen + 1 > done THEN reading.gen + 1 ELSE done
  /\ pc' = 0
  /\ UNCHANGED <<gen, open, closed, reading, mem, pending, deferred, guest, waitgen, torn>>

Next == Upload \/ Flush \/ Lock \/ WaitDone \/ Write \/ Destroy \/ FreeDeferred
        \/ Poll \/ Run \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Poll \/ Run \/ Done) /\ WF_vars(WaitDone)

\* ---- properties ----

TypeOK ==
  /\ gen \in 0..MaxGen /\ done \in 0..MaxGen /\ mem \in 0..MaxVer
  /\ guest \in {"idle", "wait", "write"}
  /\ \A i \in DOMAIN closed : closed[i].gen < gen
  /\ done <= gen

NoTornUpload == ~torn

\* batches are taken in order, and `done` never passes what the page ran
DoneInOrder == reading.gen # None => done <= reading.gen + 1

\* the guest never waits forever for the page (the page is fair)
WaitEnds == (guest = "wait") ~> (guest # "wait")
=============================================================================
