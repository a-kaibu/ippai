---------------------------- MODULE MfEngineLife ----------------------------
(* An IMFMediaEngine's life (crates/sake/src/mf.rs: en_add_ref, en_release, en_shutdown,
   events_thread, next_event, retire; review b08).

   The game holds the engine (refs), posts events (State::event through `engine`), shuts it
   down and releases it; en_release at 0 shuts it down and then calls retire, each step under
   the ENGINES lock. Its events thread looks under the lock: no entry, it ends; shut, it ends
   (thread := FALSE, then retire); an event, it calls EventNotify; nothing, it sleeps on
   EVENTS_CV (the check and the sleep atomic, as Condvar::wait_timeout) until `engine`
   notifies (an event, or the shutdown) or the wait times out.

   retire removes the entry and frees the block only when refs = 0, the thread is gone and it
   is shut down.

   Safety (FreedOnce, FreedOnlyUnheld): the block is freed at most once, and only once nothing
   holds it. Liveness (Freed): once the game let go, the block is freed.
   Old = TRUE is the engine as it was: retire nowhere ("the block stays") - Freed is violated.
   NotifyShut = FALSE with Timeout = FALSE: a notify on events only, a thread asleep when the
   shutdown comes never ends - Freed is violated too (the timeout alone hides it, at 250 ms a
   turn). *)
EXTENDS Naturals

CONSTANTS Old, NotifyShut, Timeout, MaxRefs, MaxEvents

VARIABLES
  refs,     \* Engine::refs
  shut,     \* Engine::shut
  events,   \* Engine::st.events (how many)
  entry,    \* the engine is in ENGINES
  thread,   \* Engine::thread
  tpc,      \* the events thread: "check", "sleep", "call", "leave", "retire", "done"
  gpc,      \* the game: "run"; en_release at 0: "shutdown", then "retire"; "gone"
  freed     \* times the block was freed

vars == <<refs, shut, events, entry, thread, tpc, gpc, freed>>

Init ==
  /\ refs = 1 /\ shut = FALSE /\ events = 0 /\ entry = TRUE /\ thread = TRUE
  /\ tpc = "check" /\ gpc = "run" /\ freed = 0

\* `engine`'s notify after its closure: the thread asleep wakes
Wake(ev, sh) ==
  IF tpc = "sleep" /\ thread /\ (ev > 0 \/ (NotifyShut /\ sh)) THEN tpc' = "check" ELSE UNCHANGED tpc

Retire ==
  IF ~Old /\ entry /\ refs = 0 /\ ~thread /\ shut
  THEN /\ entry' = FALSE /\ freed' = freed + 1
  ELSE UNCHANGED <<entry, freed>>

\* ---- the game ----

AddRef ==
  /\ gpc = "run" /\ entry /\ refs > 0 /\ refs < MaxRefs
  /\ refs' = refs + 1
  /\ Wake(events, shut)
  /\ UNCHANGED <<shut, events, entry, thread, gpc, freed>>

Post ==
  /\ gpc = "run" /\ entry /\ ~shut /\ events < MaxEvents
  /\ events' = events + 1
  /\ Wake(events + 1, shut)
  /\ UNCHANGED <<refs, shut, entry, thread, gpc, freed>>

Shutdown ==
  /\ gpc = "run" /\ entry /\ refs > 0
  /\ shut' = TRUE /\ events' = 0
  /\ Wake(0, TRUE)
  /\ UNCHANGED <<refs, entry, thread, gpc, freed>>

Release ==
  /\ gpc = "run" /\ entry /\ refs > 0
  /\ refs' = refs - 1
  /\ gpc' = IF refs = 1 THEN "shutdown" ELSE "run"
  /\ Wake(events, shut)
  /\ UNCHANGED <<shut, events, entry, thread, freed>>

ReleaseShutdown ==
  /\ gpc = "shutdown"
  /\ IF entry THEN /\ shut' = TRUE /\ events' = 0 /\ Wake(0, TRUE)
     ELSE UNCHANGED <<shut, events, tpc>>
  /\ gpc' = "retire"
  /\ UNCHANGED <<refs, entry, thread, freed>>

ReleaseRetire ==
  /\ gpc = "retire"
  /\ Retire
  /\ gpc' = "gone"
  /\ UNCHANGED <<refs, shut, events, thread, tpc>>

\* ---- the events thread ----

Check ==
  /\ tpc = "check"
  /\ IF ~entry THEN /\ tpc' = "done" /\ UNCHANGED events
     ELSE IF shut THEN /\ tpc' = "leave" /\ UNCHANGED events
     ELSE IF events > 0 THEN /\ events' = events - 1 /\ tpc' = "call"
     ELSE /\ tpc' = "sleep" /\ UNCHANGED events
  /\ UNCHANGED <<refs, shut, entry, thread, gpc, freed>>

Call ==
  /\ tpc = "call"
  /\ tpc' = "check"
  /\ UNCHANGED <<refs, shut, events, entry, thread, gpc, freed>>

TimedOut ==
  /\ Timeout /\ tpc = "sleep"
  /\ tpc' = "check"
  /\ UNCHANGED <<refs, shut, events, entry, thread, gpc, freed>>

Leave ==
  /\ tpc = "leave"
  /\ thread' = FALSE
  /\ tpc' = "retire"
  /\ UNCHANGED <<refs, shut, events, entry, gpc, freed>>

ThreadRetire ==
  /\ tpc = "retire"
  /\ Retire
  /\ tpc' = "done"
  /\ UNCHANGED <<refs, shut, events, thread, gpc>>

Next ==
  \/ AddRef \/ Post \/ Shutdown \/ Release \/ ReleaseShutdown \/ ReleaseRetire
  \/ Check \/ Call \/ TimedOut \/ Leave \/ ThreadRetire

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(ReleaseShutdown) /\ WF_vars(ReleaseRetire)
  /\ WF_vars(Check) /\ WF_vars(Call) /\ WF_vars(TimedOut) /\ WF_vars(Leave) /\ WF_vars(ThreadRetire)

TypeOK ==
  /\ refs \in 0..MaxRefs /\ events \in 0..MaxEvents /\ freed \in 0..2
  /\ tpc \in {"check", "sleep", "call", "leave", "retire", "done"}
  /\ gpc \in {"run", "shutdown", "retire", "gone"}

FreedOnce == freed <= 1
FreedOnlyUnheld == freed = 1 => (refs = 0 /\ ~thread /\ ~entry)

\* once the game let go, the block goes
Freed == (gpc = "gone") ~> (freed = 1)
=============================================================================
