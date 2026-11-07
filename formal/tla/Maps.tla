--------------------------------- MODULE Maps ---------------------------------
(* What Direct3D 11 says of Map and Unmap, UpdateSubresource, ClearRenderTargetView,
   CopyResource and CopySubresourceRegion, as kura's d3d11 (with the page's executor behind it)
   must follow them; MapsTrace checks a trace of the real thing (d3d11/map_tests.rs) against it.

   Resources of 16 cells: 0 a default texture, 1 a staging texture, 2 a dynamic texture,
   3 a staging buffer, 4 a default buffer (textures 4x4 cells, buffers one row of 16). A cell
   holds a name: a write k (k), a clear k (100000 + k) or Undef.

   Map types (D3D11_MAP): 1 read, 2 write, 3 read and write, 4 write discard, 5 write no
   overwrite. Staging resources take 1 to 3, dynamic ones 4 (buffers 5 too), default ones none
   (E_INVALIDARG). A Map that reads shows the content; write discard makes it Undef; every
   other Map keeps it. A readback of the page's texture gives the content. *)
EXTENDS Integers, Sequences

Undef == -1
Res == 0..4
Cells == 0..15
Usage == <<0, 3, 2, 3, 0>>
IsTex(s) == s < 3

Empty == [content |-> [s \in Res |-> [c \in Cells |-> Undef]], ret |-> "ok", ok |-> TRUE]

In(r) == {c \in Cells : (c % 4) >= r[1] /\ (c % 4) < r[3] /\ (c \div 4) >= r[2] /\ (c \div 4) < r[4]}
\* (buffers: cells x0..x1 of the one row)
InB(r) == {c \in Cells : c >= r[1] /\ c < r[3]}
Area(s, r) == IF IsTex(s) THEN In(r) ELSE InB(r)

Seen(obs, cont) == \A c \in Cells : obs[c + 1] = -9 \/ cont[c] = Undef \/ obs[c + 1] = cont[c]

Set(cont, a, v) == [c \in Cells |-> IF c \in a THEN v ELSE cont[c]]

MapOk(s, ty) == CASE Usage[s + 1] = 3 -> ty \in 1..3
                  [] Usage[s + 1] = 2 -> ty = 4 \/ (ty = 5 /\ ~IsTex(s))
                  [] OTHER -> FALSE

\* (t.r of the source moved by t.p; buffers are a row of 16)
Moved(dst, src, t, w) ==
    [c \in Cells |-> LET x == (c % w) + t.r[1] - t.p[1]
                         y == (c \div w) + t.r[2] - t.p[2]
                     IN IF (c % w) >= t.p[1] /\ (c \div w) >= t.p[2]
                           /\ x < t.r[3] /\ y < t.r[4]
                        THEN src[y * w + x] ELSE dst[c]]

Do(st, t) ==
    LET s == t.s IN
    CASE t.op = "Map" ->
           IF ~MapOk(s, t.ty) THEN [st EXCEPT !.ret = "invalid", !.ok = TRUE]
           ELSE [st EXCEPT !.ret = "ok",
                   !.content[s] = IF t.ty = 4 THEN [c \in Cells |-> Undef] ELSE st.content[s],
                   !.ok = t.ty \in {2, 4, 5} \/ Seen(t.obs, st.content[s])]
      [] t.op = "Write" ->
           [st EXCEPT !.content[s] = Set(st.content[s], Area(s, t.r), t.k), !.ret = "ok", !.ok = TRUE]
      [] t.op = "Fill" ->
           [st EXCEPT !.content[s] = [c \in Cells |-> 100000 + t.k], !.ret = "ok", !.ok = TRUE]
      [] t.op = "Copy" ->
           [st EXCEPT !.content[t.d] = Moved(st.content[t.d], st.content[s], t, IF IsTex(s) THEN 4 ELSE 16),
                      !.ret = "ok", !.ok = TRUE]
      [] t.op = "GpuRead" ->
           [st EXCEPT !.ret = "ok", !.ok = Seen(t.obs, st.content[s])]
      [] OTHER -> [st EXCEPT !.ret = "ok", !.ok = TRUE]
=============================================================================
