-------------------------- MODULE StateBlocksTrace --------------------------
(* A trace of kura's Direct3D 9 state blocks (written by d3d9/sb_tests.rs into SbData.tla)
   against StateBlocks: every call's result and what every Get read. *)
EXTENDS StateBlocks, SbData

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ret = Trace[i].ret /\ st.got = Trace[i].got
=============================================================================
