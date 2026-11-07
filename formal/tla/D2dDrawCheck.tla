----------------------------- MODULE D2dDrawCheck -----------------------------
(* D2dDraw on its own: every call from every state, a few clips deep. *)
EXTENDS D2dDraw

VARIABLE s
Ops == {"Begin", "Draw", "Push", "Pop", "Flush", "End"}
Init == s = Empty
Next == s.clips < 4 /\ \E op \in Ops : s' = Do(s, op)
\* Properties: drawing has no clips or error left over from before BeginDraw; EndDraw succeeds
\* only from a clean, balanced drawing.
TypeOK == s.clips \in Nat /\ s.error \in {0, WrongState, PopMismatch}
Clean == ~s.drawing => s.clips = 0
EndOk == Do(s, "End").ret = 0 <=> (s.drawing /\ s.error = 0 /\ s.clips = 0)
=============================================================================
