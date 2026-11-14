----------------------------- MODULE SendEnd -----------------------------
(* SendMessage to a window of another thread while that thread ends (crates/sake/src/user.rs:
   send_other_thread, process_sent, thread_gone; k32::exit_thread calls thread_gone).

   A sender queues its message on the target thread's queue, but only while the window is
   there, and waits for its answer. The target answers queued messages (process_sent) until it
   ends; its end takes its windows and answers everything still queued with 0, under the same
   USER lock as the queueing.

   Liveness (EveryoneReturns): every SendMessage returns. Safety (Queued): a waiting sender is
   queued or answered; after the end nothing stays queued.
   Old = TRUE is sake as it was: a message was queued whether the window was there or not
   (queues.entry().or_default()), the end dropped nothing, and the sender only gave up when
   the thread had neither windows nor a queue, which never happened: it waited forever. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Senders, Old, MaxSends

VARIABLES
  alive,     \* the target thread runs and its window exists
  queue,     \* senders whose message is queued on its queue
  answered,  \* senders whose answer is in
  spc,       \* each sender: "idle", "waiting", "done"
  sends      \* SendMessage calls made per sender

vars == <<alive, queue, answered, spc, sends>>

Init ==
  /\ alive = TRUE /\ queue = {} /\ answered = {}
  /\ spc = [s \in Senders |-> "idle"] /\ sends = [s \in Senders |-> 0]

\* ---- a sender ----

Send(s) ==
  /\ spc[s] = "idle" /\ sends[s] < MaxSends
  /\ sends' = [sends EXCEPT ![s] = @ + 1]
  /\ answered' = answered \ {s}
  /\ IF alive \/ Old
       THEN /\ queue' = queue \cup {s} /\ spc' = [spc EXCEPT ![s] = "waiting"]
       ELSE /\ spc' = [spc EXCEPT ![s] = "done"] /\ UNCHANGED queue
  /\ UNCHANGED alive

\* (the answer, or the old give-up check: no windows and no queue of that thread)
Return(s) ==
  /\ spc[s] = "waiting"
  /\ s \in answered \/ (~Old /\ ~alive /\ s \notin queue)
  /\ spc' = [spc EXCEPT ![s] = "done"]
  /\ UNCHANGED <<alive, queue, answered, sends>>

Again(s) ==
  /\ spc[s] = "done"
  /\ spc' = [spc EXCEPT ![s] = "idle"]
  /\ UNCHANGED <<alive, queue, answered, sends>>

\* ---- the target thread ----

Answer(s) ==
  /\ alive /\ s \in queue
  /\ queue' = queue \ {s}
  /\ answered' = answered \cup {s}
  /\ UNCHANGED <<alive, spc, sends>>

End ==
  /\ alive
  /\ alive' = FALSE
  /\ IF Old THEN UNCHANGED <<queue, answered>>
     ELSE /\ answered' = answered \cup queue /\ queue' = {}
  /\ UNCHANGED <<spc, sends>>

Next ==
  \/ \E s \in Senders : Send(s) \/ Return(s) \/ Again(s) \/ Answer(s)
  \/ End

Spec == Init /\ [][Next]_vars /\ \A s \in Senders : WF_vars(Return(s)) /\ WF_vars(Answer(s))

Queued ==
  /\ \A s \in Senders : spc[s] = "waiting" => s \in queue \/ s \in answered \/ ~alive
  /\ (~Old /\ ~alive) => queue = {}

EveryoneReturns == \A s \in Senders : spc[s] = "waiting" ~> spc[s] = "done"
=============================================================================
