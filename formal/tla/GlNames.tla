------------------------------- MODULE GlNames -------------------------------
(* What OpenGL says of object names, as kura's GL (gl/mod.rs) must follow them on one context;
   GlNamesTrace checks a trace of the real thing (gl/names_tests.rs) against it.

   Each kind (textures, buffers, framebuffers, renderbuffers) has its own names. glGen* gives
   names in use by nothing of its kind, none 0; they are reserved, and an object exists from
   the first glBind* of the name on (glIs* is true from then). glBind* of a name nobody gave
   makes the object too (the compatibility profile). glDelete* frees the names; bindings of a
   deleted object in the context go back to 0, a deleted framebuffer's too; names in use by
   nothing are skipped. A negative count is GL_INVALID_VALUE (0x501). Attaching to framebuffer
   0, or a texture or renderbuffer that does not exist, is GL_INVALID_OPERATION (0x502). The
   first error is kept until glGetError.

   Left out on purpose, as kura does it: a deleted texture or renderbuffer is detached from
   every framebuffer (GL detaches it from the bound ones only, and the others keep it alive
   without its name). *)
EXTENDS Integers, Sequences, FiniteSets, TLC

Kinds == {"tex", "buf", "fbo", "rb"}
Targets == {"array", "element", "draw", "read", "rb"}

Empty == [used |-> [k \in Kinds |-> {}], obj |-> [k \in Kinds |-> {}],
          b |-> [x \in Targets |-> 0], units |-> <<0, 0>>, active |-> 0,
          att |-> <<>>, error |-> 0, ok |-> TRUE]

Err(st, e) == IF st.error = 0 THEN e ELSE st.error

Range(s) == {s[i] : i \in DOMAIN s}

\* (attachments of every framebuffer, with `f` applied)
MapAtt(att, f(_)) == [x \in DOMAIN att |-> f(att[x])]

\* (what deleting object `id` of kind `k` unbinds)
Gone(st, k, id) ==
    CASE k = "tex" -> [st EXCEPT !.units = [u \in 1..2 |-> IF st.units[u] = id THEN 0 ELSE st.units[u]],
                                 !.att = MapAtt(st.att, LAMBDA a : IF a[1] = id THEN <<0, a[2]>> ELSE a)]
      [] k = "buf" -> [st EXCEPT !.b = [x \in Targets |-> IF x \in {"array", "element"} /\ st.b[x] = id THEN 0 ELSE st.b[x]]]
      [] k = "fbo" -> [st EXCEPT !.b = [x \in Targets |-> IF x \in {"draw", "read"} /\ st.b[x] = id THEN 0 ELSE st.b[x]],
                                 !.att = [x \in DOMAIN st.att \ {id} |-> st.att[x]]]
      [] OTHER     -> [st EXCEPT !.b = [x \in Targets |-> IF x = "rb" /\ st.b[x] = id THEN 0 ELSE st.b[x]],
                                 !.att = MapAtt(st.att, LAMBDA a : IF a[2] = id THEN <<a[1], 0>> ELSE a)]

RECURSIVE DeleteAll(_, _, _)
DeleteAll(st, k, s) ==
    IF s = <<>> THEN st
    ELSE LET id == Head(s)
             st2 == IF id = 0 \/ id \notin st.used[k] THEN st
                    ELSE LET st1 == [st EXCEPT !.used[k] = @ \ {id}, !.obj[k] = @ \ {id}]
                         IN IF id \in st.obj[k] THEN Gone(st1, k, id) ELSE st1
         IN DeleteAll(st2, k, Tail(s))

Bind(st, t) ==
    LET k == t.kind
        id == t.v
        st1 == IF id = 0 THEN st
               ELSE [st EXCEPT !.used[k] = @ \cup {id}, !.obj[k] = @ \cup {id},
                               !.att = IF k = "fbo" /\ id \notin DOMAIN st.att
                                       THEN st.att @@ (id :> <<0, 0>>) ELSE st.att]
    IN CASE t.t = "tex"  -> [st1 EXCEPT !.units[st.active + 1] = id]
         [] t.t = "both" -> [st1 EXCEPT !.b["draw"] = id, !.b["read"] = id]
         [] OTHER        -> [st1 EXCEPT !.b[t.t] = id]

Do(st0, t) ==
    LET st == [st0 EXCEPT !.ok = TRUE] IN
    CASE t.op = "Gen" ->
           IF t.v < 0 THEN [st EXCEPT !.error = Err(st, 1281)]
           ELSE [st EXCEPT !.used[t.kind] = @ \cup Range(t.names),
                   !.ok = Len(t.names) = t.v /\ Cardinality(Range(t.names)) = t.v
                          /\ \A n \in Range(t.names) : n # 0 /\ n \notin st.used[t.kind]]
      [] t.op = "Delete" ->
           LET st1 == IF t.v < 0 THEN [st EXCEPT !.error = Err(st, 1281)] ELSE st
           IN DeleteAll(st1, t.kind, t.names)
      [] t.op = "Bind" -> Bind(st, t)
      [] t.op = "Active" -> [st EXCEPT !.active = t.v]
      [] t.op = "Attach" ->
           LET fb == st.b["draw"]
               k == IF t.t = "color" THEN "tex" ELSE "rb"
           IN IF fb = 0 \/ (t.v # 0 /\ t.v \notin st.obj[k]) THEN [st EXCEPT !.error = Err(st, 1282)]
              ELSE [st EXCEPT !.att[fb] = IF t.t = "color" THEN <<t.v, @[2]>> ELSE <<@[1], t.v>>]
      [] t.op = "Is" -> [st EXCEPT !.ok = (t.got = IF t.v \in st.obj[t.kind] THEN 1 ELSE 0)]
      [] t.op = "Get" ->
           [st EXCEPT !.ok = t.got = CASE t.t = "tex" -> st.units[st.active + 1]
                                       [] t.t = "active" -> st.active
                                       [] OTHER -> st.b[t.t]]
      [] t.op = "GetAttach" ->
           [st EXCEPT !.ok = t.got = (IF t.t = "color" THEN st.att[st.b["draw"]][1] ELSE st.att[st.b["draw"]][2])]
      [] t.op = "GetError" -> [st EXCEPT !.ok = t.got = st.error, !.error = 0]
      [] OTHER -> st
=============================================================================
