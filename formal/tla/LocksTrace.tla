----------------------------- MODULE LocksTrace -----------------------------
(* A trace of kura's surface locks and copies (written by d3d9/lock_tests.rs into LockData.tla)
   against Locks: every call's result, and what each lock and page readback showed. *)
EXTENDS Locks, LockData

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ret = Trace[i].ret /\ st.ok
=============================================================================
