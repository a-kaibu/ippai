---------------------------- MODULE ComRefsTrace ----------------------------
(* A trace of kura's object tables (written by the d3d9/d3d11 trace tests into ComTrace.tla)
   against ComRefs: after every call, the result, the object handed back and which of the
   objects seen so far are alive must be what ComRefs says. *)
EXTENDS ComRefs, ComTrace

VARIABLES i, st

Init == i = 0 /\ st = Empty
Step == i < Len(Trace) /\ i' = i + 1 /\ st' = Do(st, Trace[i + 1])
End == i = Len(Trace) /\ UNCHANGED <<i, st>>
Next == Step \/ End
Spec == Init /\ [][Next]_<<i, st>>

\* (the call that differs, and what ComRefs expected)
Matches == i > 0 => /\ st.ret = Trace[i].ret
                    /\ st.got = Trace[i].got
                    /\ st.alive = Trace[i].alive
=============================================================================
