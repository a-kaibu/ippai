------------------------------ MODULE SnapGpu ------------------------------
(* A save state's GPU read-back (crates/kura/src/snap.rs `take`, render/mod.rs `snap_poll`):
   the coordinator asks the drawing thread for a dump (VNHOST_SNAP.gpu = DUMP), waits up to
   GPU_TIMEOUT for DONE and gives up otherwise (resume_all: NONE); the drawing thread starts a
   dump when it sees DUMP, and stores DONE when the dump is ready (its contents in SNAP_GPU,
   the state the page then copies).

   Numbered = FALSE is the handshake as it was: the word is only the state. A dump the coordinator
   gave up on stays started (`snapping`), and the next save's DUMP finishes it: its old
   contents are taken for the new save; or its DONE, stored after the coordinator gave up and
   asked again, answers the new request before that dump ran.
   Numbered = TRUE is the handshake now: the word is (save number, state); the drawing thread
   remembers which save its dump is for, finishes an older one without publishing it, and
   publishes and answers by compare-and-swap from DUMP of that same save only.

   Safe: every save the coordinator accepts holds the dump made for it.
   Live (Numbered): a save whose dump comes in time is accepted (no stale word blocks it). *)
EXTENDS Naturals, Sequences

CONSTANTS Numbered, MaxSaves

VARIABLES word, cseq, cstate, snapping, rseq, pending, published, accepted

vars == <<word, cseq, cstate, snapping, rseq, pending, published, accepted>>

None == [n |-> 0, st |-> "none"]

Init ==
    /\ word = None
    /\ cseq = 0
    /\ cstate = "idle"
    /\ snapping = FALSE
    /\ rseq = 0
    /\ pending = FALSE
    /\ published = 0
    /\ accepted = <<>>

\* ---- the coordinator

Request ==
    /\ cstate = "idle" /\ cseq < MaxSaves
    /\ cseq' = cseq + 1
    /\ word' = [n |-> cseq + 1, st |-> "dump"]
    /\ cstate' = "waiting"
    /\ UNCHANGED <<snapping, rseq, pending, published, accepted>>

Answered == IF Numbered THEN word = [n |-> cseq, st |-> "done"] ELSE word.st = "done"

Accept ==
    /\ cstate = "waiting" /\ Answered
    /\ accepted' = Append(accepted, <<cseq, published>>)
    /\ word' = None
    /\ cstate' = "idle"
    /\ UNCHANGED <<cseq, snapping, rseq, pending, published>>

GiveUp ==
    /\ cstate = "waiting"
    /\ word' = None
    /\ cstate' = "idle"
    /\ UNCHANGED <<cseq, snapping, rseq, pending, published, accepted>>

\* ---- the drawing thread

Begin ==
    /\ word.st = "dump" /\ ~snapping /\ ~pending
    /\ snapping' = TRUE
    /\ rseq' = word.n
    /\ UNCHANGED <<word, cseq, cstate, pending, published, accepted>>

\* (Numbered: a dump for an older save is finished and dropped before the new one starts)
Drop ==
    /\ Numbered /\ snapping /\ word.st = "dump" /\ word.n # rseq
    /\ snapping' = FALSE
    /\ UNCHANGED <<word, cseq, cstate, rseq, pending, published, accepted>>

\* (Numbered: ready; published and answered only if the word still asks for this save's dump)
FinishCas ==
    /\ Numbered /\ snapping
    /\ snapping' = FALSE
    /\ IF word = [n |-> rseq, st |-> "dump"]
       THEN /\ published' = rseq
            /\ word' = [n |-> rseq, st |-> "done"]
       ELSE UNCHANGED <<published, word>>
    /\ UNCHANGED <<cseq, cstate, rseq, pending, accepted>>

\* (as it was: ready when it looks and sees DUMP; the store follows the look)
FinishLook ==
    /\ ~Numbered /\ snapping /\ word.st = "dump"
    /\ snapping' = FALSE
    /\ pending' = TRUE
    /\ UNCHANGED <<word, cseq, cstate, rseq, published, accepted>>

FinishStore ==
    /\ ~Numbered /\ pending
    /\ pending' = FALSE
    /\ published' = rseq
    /\ word' = [word EXCEPT !.st = "done"]
    /\ UNCHANGED <<cseq, cstate, snapping, rseq, accepted>>

Next == Request \/ Accept \/ GiveUp \/ Begin \/ Drop \/ FinishCas \/ FinishLook \/ FinishStore

Spec == Init /\ [][Next]_vars /\ WF_vars(Begin \/ Drop \/ FinishCas \/ FinishLook \/ FinishStore) /\ WF_vars(Accept)

Safe == \A i \in 1..Len(accepted) : accepted[i][1] = accepted[i][2]

\* (once the drawing thread is done and the coordinator still waits, it is answered)
Live == [](cstate = "waiting" /\ word.st = "dump" => <>(cstate # "waiting"))
=============================================================================
