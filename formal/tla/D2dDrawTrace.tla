---------------------------- MODULE D2dDrawTrace ----------------------------
(* A trace of sake's D2D drawing state (written by crates/sake/src/d2d/draw_tests.rs into
   D2dData.tla) against D2dDraw: what every call gave back. *)
EXTENDS D2dDraw, D2dData, Sequences

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1].op)
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ret = Trace[i].ret
=============================================================================
