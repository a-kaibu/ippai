---------------------------- MODULE SnapPause ----------------------------
(* Save states: how every thread comes to stand still before the page copies the memory, and
   goes on after (crates/kura/src/snap.rs).

   Guest threads run translated code and look at `pause` at the top of their run loop (or in a
   wait: sake's snap::point); asked, they `park`: take GATE, say PARKED, wait on GATE_CV while
   the pause lasts (every wake from that wait counts in STIRS), then say BUSY. Host threads
   (mixer, fetchers, the drawing thread) say IDLE before they wait for work and call `busy`
   when work comes: BUSY, then a look at `pause`; asked, they stand still as a guest does.
   The coordinator (`take`) sets `pause`, wakes the sleepers, waits until every thread but
   the idle host ones is PARKED (blockers(true)), then until those are too (blockers(false),
   waking them meanwhile), takes GATE once (every thread that said PARKED is in its wait
   then), says READY and waits while the page copies, then RESUME:
   resume_all clears `pause` under GATE and notifies, and waits (up to 2 s) until no thread
   is PARKED any more. A thread that has not reached a safe point in 5 s fails the save
   (resume_all too).

   Safety (Still): while the page copies, no thread runs its work, holds GATE or is between
   two looks at the pause; Unstirred: no thread woke from its stand-still wait meanwhile
   (STIRS unchanged: the page keeps the state).
   Liveness: every save ends (the coordinator comes back to rest) and every thread works again.

   Spurious = TRUE lets GATE_CV wake a waiter without a notify (futex-based condition
   variables may): Unstirred breaks, and snap.rs detects it (Ctl::stirred, the page drops the
   state). NoFence = TRUE is `take` as it was, READY without taking GATE: a thread that said
   PARKED but had not reached its wait held GATE in the copy. GiveUp = TRUE lets resume_all stop waiting for the parked threads to wake (its 2 s):
   one still in its wait when the next pause starts stands as PARKED for that one and wakes
   while the page copies ("stirred left 3" in the snap test), detected the same way. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Guests, Hosts, Spurious, GiveUp, NoFence, MaxSaves

Threads == Guests \cup Hosts
None == "none"

VARIABLES
  pause,     \* Ctl::pause
  st,        \* per thread: Part::state (BUSY, IDLE, PARKED)
  pc,        \* per thread: where it is
  gate,      \* who holds GATE (None: nobody)
  sleeping,  \* threads in GATE_CV.wait (GATE released)
  woken,     \* threads a notify (or a spurious wake) took out of the wait, to take GATE again
  stirs,     \* STIRS
  coord,     \* the coordinator's step
  stirs0,    \* STIRS when READY was said
  saves      \* saves begun (bounds the model)

vars == <<pause, st, pc, gate, sleeping, woken, stirs, coord, stirs0, saves>>

Init ==
  /\ pause = 0
  /\ st = [t \in Threads |-> "BUSY"]
  /\ pc = [t \in Threads |-> IF t \in Guests THEN "run" ELSE "work"]
  /\ gate = None /\ sleeping = {} /\ woken = {}
  /\ stirs = 0 /\ coord = "rest" /\ stirs0 = 0 /\ saves = 0

\* ---- standing still (park / busy's loop: stand_still) ----

\* takes GATE, says PARKED (park; busy counts a stir first)
Lock(t, from) ==
  /\ pc[t] = from /\ gate = None
  /\ gate' = t /\ st' = [st EXCEPT ![t] = "PARKED"]
  /\ pc' = [pc EXCEPT ![t] = "stand"]
  /\ UNCHANGED <<pause, sleeping, woken, stirs, coord, stirs0, saves>>

\* stand_still: `while pausing() wait`; the pause over: lets go of GATE, says BUSY
Stand(t) ==
  /\ pc[t] = "stand" /\ gate = t
  /\ IF pause = 1
       THEN /\ sleeping' = sleeping \cup {t} /\ gate' = None
            /\ pc' = [pc EXCEPT ![t] = "wait"]
            /\ UNCHANGED st
       ELSE /\ gate' = None /\ UNCHANGED sleeping
            /\ st' = [st EXCEPT ![t] = "BUSY"]
            /\ pc' = [pc EXCEPT ![t] = IF t \in Guests THEN "run" ELSE "check"]
  /\ UNCHANGED <<pause, woken, stirs, coord, stirs0, saves>>

\* out of GATE_CV.wait: GATE again, one more stir
Wake(t) ==
  /\ pc[t] = "wait" /\ t \in woken /\ gate = None
  /\ gate' = t /\ woken' = woken \ {t} /\ stirs' = stirs + 1
  /\ pc' = [pc EXCEPT ![t] = "stand"]
  /\ UNCHANGED <<pause, st, sleeping, coord, stirs0, saves>>

SpuriousWake(t) ==
  /\ Spurious /\ t \in sleeping
  /\ sleeping' = sleeping \ {t} /\ woken' = woken \cup {t}
  /\ UNCHANGED <<pause, st, pc, gate, stirs, coord, stirs0, saves>>

\* ---- guest threads ----

\* the run loop's top: a block of translated code, or a park when asked
Run(g) ==
  /\ pc[g] = "run"
  /\ IF pause = 0
       THEN UNCHANGED pc   \* (its work: changes memory)
       ELSE pc' = [pc EXCEPT ![g] = "park"]
  /\ UNCHANGED <<pause, st, gate, sleeping, woken, stirs, coord, stirs0, saves>>

\* ---- host threads ----

\* done with a piece of work: idle(), then its own wait for more
Idle(h) ==
  /\ pc[h] = "work"
  /\ st' = [st EXCEPT ![h] = "IDLE"] /\ pc' = [pc EXCEPT ![h] = "sleep"]
  /\ UNCHANGED <<pause, gate, sleeping, woken, stirs, coord, stirs0, saves>>

\* work came (or its wait timed out, or a pause woke it): busy() says BUSY first
Busy(h) ==
  /\ pc[h] = "sleep"
  /\ st' = [st EXCEPT ![h] = "BUSY"] /\ pc' = [pc EXCEPT ![h] = "check"]
  /\ UNCHANGED <<pause, gate, sleeping, woken, stirs, coord, stirs0, saves>>

\* busy()'s look at the pause: work, or a stir and a stand-still
Check(h) ==
  /\ pc[h] = "check"
  /\ IF pause = 0
       THEN pc' = [pc EXCEPT ![h] = "work"] /\ UNCHANGED stirs
       ELSE pc' = [pc EXCEPT ![h] = "busylock"] /\ stirs' = stirs + 1
  /\ UNCHANGED <<pause, st, gate, sleeping, woken, coord, stirs0, saves>>

\* ---- the coordinator ----

Blockers(idleOk) == {t \in Threads : st[t] /= "PARKED" /\ ~(idleOk /\ st[t] = "IDLE" /\ t \in Hosts)}

Start ==
  /\ coord = "rest" /\ saves < MaxSaves
  /\ pause' = 1 /\ coord' = "stop1" /\ saves' = saves + 1
  /\ UNCHANGED <<st, pc, gate, sleeping, woken, stirs, stirs0>>

\* every thread but the idle host ones parked (wake_all meanwhile wakes sleepers)
Stop1 ==
  /\ coord = "stop1" /\ Blockers(TRUE) = {}
  /\ coord' = "stop2"
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, stirs0, saves>>

\* the idle ones too: woken, they come to busy() and stand still there
Stop2 ==
  /\ coord = "stop2" /\ Blockers(FALSE) = {}
  /\ IF NoFence
       THEN coord' = "ready" /\ stirs0' = stirs
       ELSE coord' = "fence" /\ UNCHANGED stirs0
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, saves>>

\* GATE taken and let go once: a thread says PARKED holding GATE and lets go of it only in its
\* wait, so every thread that said PARKED is in its wait now (NoFence: READY at once, and a
\* thread between its PARKED and its wait holds GATE in the copy, for ever in the restored
\* module)
Fence ==
  /\ coord = "fence" /\ gate = None
  /\ coord' = "ready" /\ stirs0' = stirs
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, saves>>

\* a host thread waiting for work is woken by wake_all (vfs::snap_wake, display::snap_wake...)
WakeSleeper(h) ==
  /\ coord \in {"stop1", "stop2"} /\ pc[h] = "sleep"
  /\ Busy(h)

\* the 5 s give-up: the save fails, everyone goes on
Fail ==
  /\ coord \in {"stop1", "stop2"}
  /\ coord' = "resume"
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, stirs0, saves>>

\* the page has its copy: RESUME
Copied ==
  /\ coord = "ready"
  /\ coord' = "resume"
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, stirs0, saves>>

\* resume_all: under GATE, pause 0 and notify_all
Resume ==
  /\ coord = "resume" /\ gate = None
  /\ pause' = 0 /\ woken' = woken \cup sleeping /\ sleeping' = {}
  /\ coord' = "settle"
  /\ UNCHANGED <<st, pc, gate, stirs, stirs0, saves>>

\* until no thread is PARKED (or, GiveUp, its 2 s are over)
Settle ==
  /\ coord = "settle"
  /\ (\A t \in Threads : st[t] /= "PARKED") \/ GiveUp
  /\ coord' = "rest"
  /\ UNCHANGED <<pause, st, pc, gate, sleeping, woken, stirs, stirs0, saves>>

Thread(t) ==
  \/ t \in Guests /\ (Run(t) \/ Lock(t, "park"))
  \/ t \in Hosts /\ (Idle(t) \/ Busy(t) \/ Check(t) \/ Lock(t, "busylock"))
  \/ Stand(t) \/ Wake(t) \/ SpuriousWake(t)

Next ==
  \/ \E t \in Threads : Thread(t)
  \/ \E h \in Hosts : WakeSleeper(h)
  \/ Start \/ Stop1 \/ Stop2 \/ Fence \/ Fail \/ Copied \/ Resume \/ Settle

\* (fair: the threads and the coordinator's steps; not the give-up, not spurious wakes; a host
\* thread's piece of work ends sometime, and so does its own wait: more work or its timeout)
Fairness ==
  /\ \A t \in Threads : WF_vars(Stand(t)) /\ WF_vars(Wake(t)) /\ WF_vars(Lock(t, "park")) /\ WF_vars(Lock(t, "busylock"))
  /\ \A g \in Guests : WF_vars(Run(g))
  /\ \A h \in Hosts : WF_vars(Check(h)) /\ WF_vars(WakeSleeper(h)) /\ WF_vars(Busy(h)) /\ WF_vars(Idle(h))
  /\ WF_vars(Stop1) /\ WF_vars(Stop2) /\ WF_vars(Fence) /\ WF_vars(Copied) /\ WF_vars(Resume) /\ WF_vars(Settle)

Spec == Init /\ [][Next]_vars /\ Fairness

\* ---- properties ----

TypeOK ==
  /\ pause \in {0, 1}
  /\ \A t \in Threads : st[t] \in {"BUSY", "IDLE", "PARKED"}
  /\ gate \in Threads \cup {None}
  /\ sleeping \subseteq Threads /\ woken \subseteq Threads /\ sleeping \cap woken = {}

\* while the page copies: everyone in its stand-still wait, GATE free
Still ==
  coord = "ready" =>
    /\ \A t \in Threads : pc[t] = "wait"
    /\ gate = None

Unstirred == coord = "ready" => stirs = stirs0

\* a thread that says PARKED stands still (never runs its work)
ParkedStands == \A t \in Threads : st[t] = "PARKED" => pc[t] \in {"stand", "wait"}

Safe == TypeOK /\ Still /\ Unstirred /\ ParkedStands

\* every save ends; every thread gets to work again after it
SavesEnd == coord /= "rest" ~> coord = "rest"
WorkAgain == \A t \in Threads : (pc[t] = "wait") ~> (pc[t] \in {"run", "work"})
=============================================================================
