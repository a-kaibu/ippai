----------------------------- MODULE StopCalls -----------------------------
(* A source filter's streaming thread and a game that stops the graph and takes its objects
   apart (crates/waits/src/calls.rs, crates/sake/src/dshow_mpeg.rs; #230).

   The thread looks at the epoch, then asks to enter with it (Calls::enter: refused once the
   epoch moved on, or while stopped), calls into the game's object (SyncRead,
   Receive) and leaves. The game's Stop moves the epoch on and waits while a call is in
   flight (Calls::stop), then the game destroys the object; later it may run again.

   Safety (NoCallIntoTheDead): no call is in flight into a destroyed object.
   Old = TRUE is the thread as it was: it called after its look with no enter, and Stop did
   not wait (the krmovie reader freed under SyncRead). GiveUp = TRUE lets Stop stop waiting
   (its timeout): the same use after free, the price of not hanging forever on a call that
   never comes back. *)
EXTENDS Naturals

CONSTANTS Old, GiveUp, MaxRuns

VARIABLES
  epoch,     \* Calls::epoch
  inside,    \* Calls::inside
  running,   \* Calls::running (run .. stop)
  seen,      \* the epoch the thread looked at
  tpc,       \* the thread: "look", "enter", "call", "leave"
  gpc,       \* the game: "run", "stop", "wait", "destroy", "dead"
  alive,     \* the game's object exists
  runs

vars == <<epoch, inside, running, seen, tpc, gpc, alive, runs>>

Init ==
  /\ epoch = 0 /\ inside = FALSE /\ running = TRUE /\ seen = 0
  /\ tpc = "look" /\ gpc = "run" /\ alive = TRUE /\ runs = 0

\* ---- the thread ----

\* (as it was, it looked at the filter's state: on while it runs)
Look ==
  /\ tpc = "look" /\ (Old => gpc = "run")
  /\ seen' = epoch
  /\ tpc' = "enter"
  /\ UNCHANGED <<epoch, inside, running, gpc, alive, runs>>

Enter ==
  /\ tpc = "enter"
  /\ IF Old THEN /\ tpc' = "call" /\ UNCHANGED inside
     ELSE IF epoch = seen /\ running /\ ~inside
          THEN /\ inside' = TRUE /\ tpc' = "call"
          ELSE /\ tpc' = "look" /\ UNCHANGED inside
  /\ UNCHANGED <<epoch, running, seen, gpc, alive, runs>>

Call ==
  /\ tpc = "call"
  /\ tpc' = "leave"
  /\ UNCHANGED <<epoch, inside, running, seen, gpc, alive, runs>>

Leave ==
  /\ tpc = "leave"
  /\ inside' = FALSE
  /\ tpc' = "look"
  /\ UNCHANGED <<epoch, running, seen, gpc, alive, runs>>

\* ---- the game ----

Stop ==
  /\ gpc = "run"
  /\ epoch' = (epoch + 1) % 4
  /\ running' = FALSE
  /\ gpc' = "wait"
  /\ UNCHANGED <<inside, seen, tpc, alive, runs>>

Waited ==
  /\ gpc = "wait"
  /\ (Old \/ GiveUp \/ ~inside)
  /\ gpc' = "destroy"
  /\ UNCHANGED <<epoch, inside, running, seen, tpc, alive, runs>>

Destroy ==
  /\ gpc = "destroy"
  /\ alive' = FALSE
  /\ gpc' = "dead"
  /\ UNCHANGED <<epoch, inside, running, seen, tpc, runs>>

\* (a new object, run again)
Again ==
  /\ gpc = "dead" /\ runs < MaxRuns
  /\ alive' = TRUE /\ gpc' = "run" /\ runs' = runs + 1 /\ running' = TRUE
  /\ UNCHANGED <<epoch, inside, seen, tpc>>

Next == Look \/ Enter \/ Call \/ Leave \/ Stop \/ Waited \/ Destroy \/ Again

\* (the call is "in flight" from Call's step until Leave: tpc = "leave" means it went out)
NoCallIntoTheDead == tpc \in {"call", "leave"} => alive

Spec == Init /\ [][Next]_vars
=============================================================================
