-------------------------------- MODULE Locks --------------------------------
(* What Direct3D 9 says of LockRect and UnlockRect, ColorFill, GetRenderTargetData,
   UpdateSurface and StretchRect, as kura's d3d9 (with the page's executor behind it) must
   follow them; LocksTrace checks a trace of the real thing (d3d9/lock_tests.rs) against it.

   Five surfaces of 4x4 cells: 0 a managed texture's level, 1 an off-screen plain surface in
   D3DPOOL_DEFAULT, 2 and 4 render targets, 3 a system memory surface. A cell holds a name: a
   write k (k), a fill k (100000 + k) or Undef (what the application may not count on: a new
   surface, a rectangle locked with D3DLOCK_DISCARD).

   A lock fails on a locked surface; an unlock on an unlocked one. While locked, the memory a
   lock gave holds the content of its rectangle (but D3DLOCK_DISCARD's), and writes there are
   the content once unlocked. ColorFill fails but on D3DPOOL_DEFAULT surfaces. A readback of
   the page's texture of an unlocked surface gives its content. *)
EXTENDS Integers, Sequences

Undef == -1
Surfaces == 0..4
Cells == 0..15
Fillable == {1, 2, 4}

Empty == [content |-> [s \in Surfaces |-> [c \in Cells |-> Undef]],
          locked |-> [s \in Surfaces |-> FALSE], ret |-> "ok", ok |-> TRUE]

\* (the cells of rectangle r = <<x0, y0, x1, y1>>)
In(r) == {c \in Cells : (c % 4) >= r[1] /\ (c % 4) < r[3] /\ (c \div 4) >= r[2] /\ (c \div 4) < r[4]}

\* (what the application read agrees with the content where it is defined)
Seen(obs, cont, r) == \A c \in In(r) : cont[c] = Undef \/ obs[c + 1] = cont[c]

Set(cont, r, v) == [c \in Cells |-> IF c \in In(r) THEN v ELSE cont[c]]

\* (t.r of the source moved by t.p)
Moved(dst, src, t) ==
    [c \in Cells |-> LET x == (c % 4) + t.r[1] - t.p[1]
                         y == (c \div 4) + t.r[2] - t.p[2]
                     IN IF (c % 4) >= t.p[1] /\ (c \div 4) >= t.p[2]
                           /\ x < t.r[3] /\ y < t.r[4]
                        THEN src[y * 4 + x] ELSE dst[c]]

Do(st, t) ==
    LET s == t.s IN
    CASE t.op = "Lock" ->
           IF st.locked[s] THEN [st EXCEPT !.ret = "invalid", !.ok = TRUE]
           ELSE [st EXCEPT !.locked[s] = TRUE, !.ret = "ok",
                   !.content[s] = IF t.kind = "discard" THEN Set(st.content[s], t.r, Undef)
                                  ELSE st.content[s],
                   !.ok = t.kind = "discard" \/ Seen(t.obs, st.content[s], t.r)]
      [] t.op = "Unlock" ->
           IF st.locked[s] THEN [st EXCEPT !.locked[s] = FALSE, !.ret = "ok", !.ok = TRUE]
           ELSE [st EXCEPT !.ret = "invalid", !.ok = TRUE]
      [] t.op = "Write" ->
           [st EXCEPT !.content[s] = Set(st.content[s], t.r, t.k), !.ret = "ok", !.ok = TRUE]
      [] t.op = "Fill" ->
           IF s \in Fillable
           THEN [st EXCEPT !.content[s] = Set(st.content[s], t.r, 100000 + t.k), !.ret = "ok", !.ok = TRUE]
           ELSE [st EXCEPT !.ret = "invalid", !.ok = TRUE]
      [] t.op = "GetRTData" ->
           [st EXCEPT !.content[t.d] = st.content[s], !.ret = "ok", !.ok = TRUE]
      [] t.op = "Copy" ->
           [st EXCEPT !.content[t.d] = Moved(st.content[t.d], st.content[s], t), !.ret = "ok", !.ok = TRUE]
      [] t.op = "GpuRead" ->
           [st EXCEPT !.ret = "ok", !.ok = Seen(t.obs, st.content[s], <<0, 0, 4, 4>>)]
      [] OTHER -> [st EXCEPT !.ret = "ok", !.ok = TRUE]
=============================================================================
