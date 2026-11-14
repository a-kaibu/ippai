---------------------------- MODULE BudgetLock ----------------------------
(* The lock word over the bucket's request budget (web/src/budget.ts withBudget, reclaim;
   host/fetch-bridge.js masuBudgetWith, masuBudgetReclaim, masuFetchDispose). Two contexts
   share it: the page (1) and its network worker (2). Each takes the word by compare-and-swap
   0 -> its tag, spinning at most MaxSpin times; only then does it write the budget, and it
   frees the word by compare-and-swap tag -> 0. The worker can stop being scheduled at any
   point (no fairness for it), and the page can terminate it: terminate() returns while the
   worker may still run a few steps ("dying") before it really stops. Only after that does the
   page take back a word the worker still held (reclaim, a second after terminate()).

   GiveUp = FALSE is the code before the review of 2026-11-14: a taker out of tries wrote the
   budget anyway and freed the word; TLC finds both inside at once. Grace = FALSE reclaims
   as soon as terminate() returns: the dying worker may still be inside. *)
EXTENDS Naturals

CONSTANTS MaxSpin, GiveUp, CasRelease, Grace

Procs == {1, 2}

VARIABLES lock, pc, n, w
vars == <<lock, pc, n, w>>

TypeOK ==
  /\ lock \in {0} \cup Procs
  /\ pc \in [Procs -> {"idle", "spin", "cs"}]
  /\ n \in [Procs -> 0..MaxSpin]
  /\ w \in {"alive", "dying", "stopped"}

Init ==
  /\ lock = 0
  /\ pc = [p \in Procs |-> "idle"]
  /\ n = [p \in Procs |-> 0]
  /\ w = "alive"

\* whether p still runs (a terminated worker may for a while)
Runs(p) == p = 1 \/ w # "stopped"

Begin(p) ==
  /\ Runs(p) /\ pc[p] = "idle"
  /\ pc' = [pc EXCEPT ![p] = "spin"]
  /\ n' = [n EXCEPT ![p] = 0]
  /\ UNCHANGED <<lock, w>>

Spin(p) ==
  /\ Runs(p) /\ pc[p] = "spin"
  /\ IF lock = 0
       THEN lock' = p /\ pc' = [pc EXCEPT ![p] = "cs"] /\ UNCHANGED <<n, w>>
     ELSE IF n[p] < MaxSpin
       THEN n' = [n EXCEPT ![p] = @ + 1] /\ UNCHANGED <<lock, pc, w>>
     ELSE IF GiveUp
       THEN pc' = [pc EXCEPT ![p] = "idle"] /\ UNCHANGED <<lock, n, w>>
       \* (before: on, into the budget without the word)
       ELSE pc' = [pc EXCEPT ![p] = "cs"] /\ UNCHANGED <<lock, n, w>>

Leave(p) ==
  /\ Runs(p) /\ pc[p] = "cs"
  /\ pc' = [pc EXCEPT ![p] = "idle"]
  /\ lock' = IF CasRelease THEN (IF lock = p THEN 0 ELSE lock) ELSE 0
  /\ UNCHANGED <<n, w>>

Terminate == w = "alive" /\ w' = "dying" /\ UNCHANGED <<lock, pc, n>>
Stop == w = "dying" /\ w' = "stopped" /\ UNCHANGED <<lock, pc, n>>

\* the page takes the dead worker's word (2 -> 1), writes the budget afresh, then frees it (Leave)
Reclaim ==
  /\ pc[1] = "idle"
  /\ IF Grace THEN w = "stopped" ELSE w # "alive"
  /\ lock = 2
  /\ lock' = 1
  /\ pc' = [pc EXCEPT ![1] = "cs"]
  /\ UNCHANGED <<n, w>>

PageNext == Begin(1) \/ Spin(1) \/ Leave(1) \/ Reclaim
Next == PageNext \/ Begin(2) \/ Spin(2) \/ Leave(2) \/ Terminate \/ Stop

\* (the page is always scheduled and reclaims when it can; a terminated worker stops in the end;
\* the worker itself has no fairness: it may be paused forever anywhere; the reclaim is a timer
\* on the page's event loop, run when the page is between tasks: strong fairness)
Spec == Init /\ [][Next]_vars /\ WF_vars(Spin(1)) /\ WF_vars(Leave(1)) /\ SF_vars(Reclaim) /\ WF_vars(Stop)

Inside(p) == pc[p] = "cs" /\ Runs(p)

\* never two writing the budget at once
MutualExclusion == ~(Inside(1) /\ Inside(2))
\* nobody writes the budget without holding the word
OwnsInside == \A p \in Procs : Inside(p) => lock = p

\* a page that tries is never stuck spinning: it gets in or gives up (tries again later)
PageGoesOn == (pc[1] = "spin") ~> (pc[1] # "spin")
\* a word held by a worker that is gone is freed in the end
DeadFreed == (w = "stopped" /\ lock = 2) ~> (lock # 2)
=============================================================================
