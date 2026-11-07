---------------------------- MODULE GlNamesTrace ----------------------------
(* A trace of kura's GL names (written by gl/names_tests.rs into GlData.tla) against GlNames:
   what every call gave back. *)
EXTENDS GlNames, GlData

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ok
=============================================================================
