---------------------------- MODULE Binds11Trace ----------------------------
(* A trace of kura's Direct3D 11 binds (written by d3d11/bind_tests.rs into BindData.tla)
   against Binds11: what every slot read back. *)
EXTENDS Binds11, BindData

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ok
=============================================================================
