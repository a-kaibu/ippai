----------------------------- MODULE MapsTrace -----------------------------
(* A trace of kura's maps and copies (written by d3d11/map_tests.rs into MapData.tla)
   against Maps: every Map's result, and what each Map and page readback showed. *)
EXTENDS Maps, MapData

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End

Matches == i > 0 => st.ret = Trace[i].ret /\ st.ok
=============================================================================
