------------------------------- MODULE D2dDraw -------------------------------
(* What Direct2D says of a render target between BeginDraw and EndDraw, as sake's device
   contexts (crates/sake/src/d2d.rs, Drawing) must follow it; D2dDrawTrace checks a trace of the
   real state machine against it, call by call.

   Drawing calls (Clear, Fill*, Draw*, PushAxisAlignedClip, PopAxisAlignedClip) draw only between
   BeginDraw and EndDraw; outside them, a second BeginDraw, or a Pop with no clip pushed, put the
   target in error: the first such error is what Flush and EndDraw return (D2DERR_WRONG_STATE,
   D2DERR_PUSH_POP_UNBALANCED for the Pop). As Windows does (winref probe d2d, "state:" lines):
   an error from drawing outside BeginDraw stays for the next BeginDraw ... EndDraw, and a Flush
   while drawing hands the error over and clears it. EndDraw with clips still pushed returns
   D2DERR_PUSH_POP_UNBALANCED; EndDraw outside drawing returns D2DERR_WRONG_STATE. EndDraw ends
   drawing whatever it returns, with no clips and no error left. *)
EXTENDS Naturals

\* (the HRESULTs less 0x88990000: TLC's integers are 32-bit)
WrongState == 1      \* 0x88990001
Unbalanced == 22     \* 0x88990016
PopMismatch == 23    \* 0x88990017

Empty == [drawing |-> FALSE, error |-> 0, clips |-> 0, ret |-> 0]

Fail(st, e) == IF st.error = 0 THEN [st EXCEPT !.error = e] ELSE st

\* (ret: what the call returns; for drawing calls 1 if it drew, popped or pushed)
Do(st, op) ==
    CASE op = "Begin" ->
            IF st.drawing THEN [Fail(st, WrongState) EXCEPT !.ret = 0]
            ELSE [drawing |-> TRUE, error |-> st.error, clips |-> 0, ret |-> 0]
      [] op = "Draw" ->
            IF st.drawing THEN [st EXCEPT !.ret = 1] ELSE [Fail(st, WrongState) EXCEPT !.ret = 0]
      [] op = "Push" ->
            IF st.drawing THEN [st EXCEPT !.clips = @ + 1, !.ret = 1]
            ELSE [Fail(st, WrongState) EXCEPT !.ret = 0]
      [] op = "Pop" ->
            IF ~st.drawing THEN [Fail(st, WrongState) EXCEPT !.ret = 0]
            ELSE IF st.clips = 0 THEN [Fail(st, Unbalanced) EXCEPT !.ret = 0]
            ELSE [st EXCEPT !.clips = @ - 1, !.ret = 1]
      [] op = "Flush" ->
            IF st.drawing THEN [st EXCEPT !.ret = st.error, !.error = 0]
            ELSE [st EXCEPT !.ret = WrongState]
      [] op = "End" ->
            [drawing |-> FALSE, error |-> 0, clips |-> 0,
             ret |-> IF ~st.drawing THEN WrongState
                     ELSE IF st.error # 0 THEN st.error
                     ELSE IF st.clips # 0 THEN Unbalanced ELSE 0]

=============================================================================
