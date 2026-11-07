----------------------------- MODULE StateBlocks -----------------------------
(* What Direct3D 9 says of state blocks, as kura's (crates/kura/src/d3d9/stateblock.rs) must
   follow it; StateBlocksTrace checks a trace of the real device (d3d9/sb_tests.rs) against
   it, call by call.

   A block holds some of the device's states, its mask: all of them (D3DSBT_ALL), the pixel
   states or the vertex states (D3DSBT_PIXELSTATE / VERTEXSTATE; Pixel and Vertex below, a few
   of each kind standing for the lists), or what was set between BeginStateBlock and
   EndStateBlock. CreateStateBlock and Capture take the masked states from the device, Apply
   puts them back and leaves the rest alone. While recording, a Set changes the recording, not
   the device (Get reads the device). Out of order: a second BeginStateBlock, EndStateBlock
   without Begin, CreateStateBlock while recording or of a type above 3: D3DERR_INVALIDCALL;
   type 0 makes a block of nothing (Windows, the winref probe d3d9sb).

   Keys: render states rsP (ALPHAREF), rsV (CULLMODE), rsB (SHADEMODE); texture stage states
   tssP (COLOROP), tssB (TEXCOORDINDEX); sampler states ssP (ADDRESSU), ssV (DMAPOFFSET:
   in D3DSBT_ALL only, not a vertex state on Windows);
   texture 0, the shaders, constant 0 of each, the view transform, the viewport, light 2.
   A key is its index in Keys; -1: not known (a block's states outside its mask). *)
EXTENDS Integers, Sequences

Keys == <<"rsP", "rsV", "rsB", "tssP", "tssB", "ssP", "ssV", "tex", "vs", "ps", "vsf", "psf", "xf", "vp", "light">>
K == 1..Len(Keys)
Named(S) == {k \in K : Keys[k] \in S}
Pixel == Named({"rsP", "rsB", "tssP", "tssB", "ssP", "ps", "psf"})
Vertex == Named({"rsV", "rsB", "tssB", "vs", "vsf", "light"})
TypeMask(t) == CASE t = 0 -> {} [] t = 1 -> K [] t = 2 -> Pixel [] t = 3 -> Vertex

\* State: the device's states; the recording (rec: on, its states, what it set); the blocks
\* (a sequence: their states and masks); the last call's result and what it gave.
Empty == [dev |-> [k \in K |-> -1], recording |-> FALSE, rst |-> [k \in K |-> -1], rmask |-> {},
          blocks |-> <<>>, ret |-> "ok", got |-> 0]

Only(st, m) == [k \in K |-> IF k \in m THEN st[k] ELSE -1]
NewBlock(s, st, m) == [s EXCEPT !.blocks = Append(@, [st |-> Only(st, m), mask |-> m]),
                                !.ret = "ok", !.got = Len(s.blocks) + 1]
Res(s, r, g) == [s EXCEPT !.ret = r, !.got = g]

\* (k: the key, the block or the type; v: the value set)
Do(s, t) ==
    CASE t.op = "Set" ->
            IF s.recording THEN Res([s EXCEPT !.rst[t.k + 1] = t.v, !.rmask = @ \cup {t.k + 1}], "ok", 0)
            ELSE Res([s EXCEPT !.dev[t.k + 1] = t.v], "ok", 0)
      [] t.op = "Get" -> Res(s, "ok", s.dev[t.k + 1])
      [] t.op = "Create" ->
            IF s.recording \/ t.k \notin 0..3 THEN Res(s, "invalid", 0)
            ELSE NewBlock(s, s.dev, TypeMask(t.k))
      [] t.op = "Begin" ->
            IF s.recording THEN Res(s, "invalid", 0)
            ELSE Res([s EXCEPT !.recording = TRUE, !.rst = s.dev, !.rmask = {}], "ok", 0)
      [] t.op = "End" ->
            IF ~s.recording THEN Res(s, "invalid", 0)
            ELSE NewBlock([s EXCEPT !.recording = FALSE], s.rst, s.rmask)
      [] t.op = "Capture" ->
            LET b == s.blocks[t.k] IN
            Res([s EXCEPT !.blocks[t.k].st = [k \in K |-> IF k \in b.mask THEN s.dev[k] ELSE b.st[k]]], "ok", 0)
      [] t.op = "Apply" ->
            LET b == s.blocks[t.k] IN
            Res([s EXCEPT !.dev = [k \in K |-> IF k \in b.mask THEN b.st[k] ELSE s.dev[k]]], "ok", 0)
=============================================================================
