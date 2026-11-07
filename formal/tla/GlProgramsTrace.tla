------------------------- MODULE GlProgramsTrace -------------------------
(* A trace of kura's GL shaders and programs (written by gl/programs_tests.rs into
   GlpData.tla) against GlPrograms: what every call gave back. *)
EXTENDS GlPrograms, GlpData, Sequences

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ok
=============================================================================
