----------------------------- MODULE GlPrograms -----------------------------
(* What OpenGL 2.0 (and ES 2.0) says of shader and program objects, as kura's GL
   (crates/kura/src/gl/glsl.rs) must follow it; GlProgramsTrace checks a trace of the real
   calls (gl/programs_tests.rs) against it, call by call.

   glDeleteShader of a shader attached to a program only flags it (DELETE_STATUS): it goes
   when no program has it. glDeleteProgram of the program in use only flags it: it goes when
   it is no longer current, and its flagged shaders with it. A name of no object is
   GL_INVALID_VALUE, a name of the other kind GL_INVALID_OPERATION; attaching twice, detaching
   what is not attached, using a program not linked: GL_INVALID_OPERATION (the program in use
   stays). A link succeeds with one compiled vertex and one compiled fragment shader attached
   (kura's restriction: one of each, GL's is one main per stage; the sources here each have
   one). The first error sticks until glGetError. One context. *)
EXTENDS Integers, FiniteSets

InvalidEnum == 1280
InvalidValue == 1281
InvalidOperation == 1282

Empty == [sh |-> [n \in {} |-> 0], pr |-> [n \in {} |-> 0], cur |-> 0, error |-> 0, ok |-> TRUE]

Shaders(s) == DOMAIN s.sh
Programs(s) == DOMAIN s.pr
Err(s, e) == IF s.error = 0 THEN [s EXCEPT !.error = e] ELSE s
\* (no object: invalid value; the other kind: invalid operation; 0 if it is one)
KindErr(s, n, shader) ==
    IF (shader /\ n \in Shaders(s)) \/ (~shader /\ n \in Programs(s)) THEN 0
    ELSE IF n \in Shaders(s) \cup Programs(s) THEN InvalidOperation ELSE InvalidValue

Restrict(f, S) == [x \in S |-> f[x]]
\* Flagged programs not in use go; then flagged shaders no program has.
Collect(s) ==
    LET ps == {p \in Programs(s) : ~s.pr[p].deleted \/ p = s.cur}
        attached == UNION {s.pr[p].att : p \in ps}
        ss == {x \in Shaders(s) : ~s.sh[x].deleted \/ x \in attached}
    IN [s EXCEPT !.pr = Restrict(@, ps), !.sh = Restrict(@, ss)]

Linkable(s, p) ==
    LET of(px) == {x \in s.pr[p].att : x \in Shaders(s) /\ s.sh[x].pixel = px}
    IN \A px \in BOOLEAN : Cardinality(of(px)) = 1 /\ \A x \in of(px) : s.sh[x].compiled

Expect(s, cond) == IF cond THEN s ELSE [s EXCEPT !.ok = FALSE]
Fresh(s, n) == n # 0 /\ n \notin Shaders(s) \cup Programs(s)

\* (t.a, t.b: names; t.got: what the call gave back)
Do(s, t) ==
    CASE t.op = "CreateShader" ->
            Expect([s EXCEPT !.sh = [x \in Shaders(s) \cup {t.got} |-> IF x = t.got
                       THEN [pixel |-> t.a = 1, compiled |-> FALSE, deleted |-> FALSE] ELSE s.sh[x]]],
                   Fresh(s, t.got))
      [] t.op = "CreateProgram" ->
            Expect([s EXCEPT !.pr = [x \in Programs(s) \cup {t.got} |-> IF x = t.got
                       THEN [att |-> {}, linked |-> FALSE, deleted |-> FALSE] ELSE s.pr[x]]],
                   Fresh(s, t.got))
      [] t.op = "Compile" ->
            IF KindErr(s, t.a, TRUE) # 0 THEN Err(s, KindErr(s, t.a, TRUE))
            ELSE [s EXCEPT !.sh[t.a].compiled = TRUE]
      [] t.op = "DeleteShader" ->
            IF t.a = 0 THEN s
            ELSE IF KindErr(s, t.a, TRUE) # 0 THEN Err(s, KindErr(s, t.a, TRUE))
            ELSE Collect([s EXCEPT !.sh[t.a].deleted = TRUE])
      [] t.op = "DeleteProgram" ->
            IF t.a = 0 THEN s
            ELSE IF KindErr(s, t.a, FALSE) # 0 THEN Err(s, KindErr(s, t.a, FALSE))
            ELSE Collect([s EXCEPT !.pr[t.a].deleted = TRUE])
      [] t.op = "Attach" ->
            LET e == IF KindErr(s, t.a, FALSE) # 0 THEN KindErr(s, t.a, FALSE)
                     ELSE IF KindErr(s, t.b, TRUE) # 0 THEN KindErr(s, t.b, TRUE)
                     ELSE IF t.b \in s.pr[t.a].att THEN InvalidOperation ELSE 0
            IN IF e # 0 THEN Err(s, e) ELSE [s EXCEPT !.pr[t.a].att = @ \cup {t.b}]
      [] t.op = "Detach" ->
            LET e == IF KindErr(s, t.a, FALSE) # 0 THEN KindErr(s, t.a, FALSE)
                     ELSE IF KindErr(s, t.b, TRUE) # 0 THEN KindErr(s, t.b, TRUE)
                     ELSE IF t.b \notin s.pr[t.a].att THEN InvalidOperation ELSE 0
            IN IF e # 0 THEN Err(s, e) ELSE Collect([s EXCEPT !.pr[t.a].att = @ \ {t.b}])
      [] t.op = "Link" ->
            IF KindErr(s, t.a, FALSE) # 0 THEN Err(s, KindErr(s, t.a, FALSE))
            ELSE [s EXCEPT !.pr[t.a].linked = Linkable(s, t.a)]
      [] t.op = "Use" ->
            IF t.a = 0 THEN Collect([s EXCEPT !.cur = 0])
            ELSE IF KindErr(s, t.a, FALSE) # 0 THEN Err(s, KindErr(s, t.a, FALSE))
            ELSE IF ~s.pr[t.a].linked THEN Err(s, InvalidOperation)
            ELSE Collect([s EXCEPT !.cur = t.a])
      [] t.op = "IsShader" -> Expect(s, t.got = IF t.a \in Shaders(s) THEN 1 ELSE 0)
      [] t.op = "IsProgram" -> Expect(s, t.got = IF t.a \in Programs(s) THEN 1 ELSE 0)
      \* (DELETE_STATUS, LINK_STATUS, COMPILE_STATUS: -1 when the call failed)
      [] t.op = "ShaderDeleted" ->
            IF KindErr(s, t.a, TRUE) # 0 THEN Expect(Err(s, KindErr(s, t.a, TRUE)), t.got = -1)
            ELSE Expect(s, t.got = IF s.sh[t.a].deleted THEN 1 ELSE 0)
      [] t.op = "Compiled" ->
            IF KindErr(s, t.a, TRUE) # 0 THEN Expect(Err(s, KindErr(s, t.a, TRUE)), t.got = -1)
            ELSE Expect(s, t.got = IF s.sh[t.a].compiled THEN 1 ELSE 0)
      [] t.op = "ProgramDeleted" ->
            IF KindErr(s, t.a, FALSE) # 0 THEN Expect(Err(s, KindErr(s, t.a, FALSE)), t.got = -1)
            ELSE Expect(s, t.got = IF s.pr[t.a].deleted THEN 1 ELSE 0)
      [] t.op = "Linked" ->
            IF KindErr(s, t.a, FALSE) # 0 THEN Expect(Err(s, KindErr(s, t.a, FALSE)), t.got = -1)
            ELSE Expect(s, t.got = IF s.pr[t.a].linked THEN 1 ELSE 0)
      [] t.op = "Attached" ->
            IF KindErr(s, t.a, FALSE) # 0 THEN Expect(Err(s, KindErr(s, t.a, FALSE)), t.got = -1)
            ELSE Expect(s, t.got = Cardinality(s.pr[t.a].att))
      [] t.op = "Current" -> Expect(s, t.got = s.cur)
      [] t.op = "GetError" -> Expect([s EXCEPT !.error = 0], t.got = s.error)
=============================================================================
