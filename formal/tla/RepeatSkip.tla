---------------------------- MODULE RepeatSkip ----------------------------
(* render/d3d.rs `repeats()`: a batch the same as the last one is not run when running it again
   would leave the same pixels. Its rule, over every batch of up to MaxLen commands on a few
   textures (one with two layers), from any starting contents: when the rule says "repeats",
   running the batch twice leaves what running it once left.

   Texture contents are terms (each subresource two pixels, so boxes can be partial): a clear
   or a fill gives a constant, a write the same bytes, a copy moves pixels, a draw gives a term
   of what it samples (every subresource of its source) and, when it blends, the old pixel.

   NewRule TRUE: the rule as it is now. FALSE: as it was before the WebGPU model tests
   (render/d3d/model_tests.rs) found its holes: clears by texture rather than subresource, no
   check of a write after a read, a read before a clear not caught. TLC must find a
   counterexample to the old one (RepeatSkipOld.cfg). *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS MaxLen, NewRule

Subs == {<<1, 1>>, <<1, 2>>, <<2, 1>>}      \* texture 1 has two layers
Texs == {1, 2}
Pix == {1, 2}
Boxes == {{1}, {1, 2}}
TexOf(s) == s[1]
SubsOf(t) == {s \in Subs : TexOf(s) = t}

Ops ==
    [k : {"clear"}, dst : Subs]
    \cup [k : {"draw"}, dst : Subs, src : Texs \cup {0}, box : Boxes, blend : BOOLEAN]
    \cup [k : {"write"}, dst : Subs, box : Boxes]
    \cup [k : {"copy"}, src : Subs, dst : Subs, box : Boxes]
    \cup [k : {"fill"}, dst : Subs, box : Boxes]

\* (a draw does not sample the texture it draws to: the executor binds white instead)
GoodOp(o) == o.k = "draw" => o.src # TexOf(o.dst)

Batches == UNION {[1..n -> {o \in Ops : GoodOp(o)}] : n \in 1..MaxLen}

S0 == [s \in Subs |-> [p \in Pix |-> <<"before", s, p>>]]

Read(st, t) == [s \in SubsOf(t) |-> st[s]]

Apply(st, o) ==
    CASE o.k = "clear" -> [st EXCEPT ![o.dst] = [p \in Pix |-> "clear"]]
      [] o.k = "fill" -> [st EXCEPT ![o.dst] = [p \in Pix |-> IF p \in o.box THEN "fill" ELSE st[o.dst][p]]]
      [] o.k = "write" -> [st EXCEPT ![o.dst] = [p \in Pix |-> IF p \in o.box THEN "bytes" ELSE st[o.dst][p]]]
      [] o.k = "copy" -> [st EXCEPT ![o.dst] = [p \in Pix |-> IF p \in o.box THEN st[o.src][p] ELSE st[o.dst][p]]]
      [] o.k = "draw" ->
            LET sampled == IF o.src = 0 THEN "none" ELSE Read(st, o.src)
            IN [st EXCEPT ![o.dst] = [p \in Pix |->
                    IF p \in o.box
                    THEN <<"draw", sampled, IF o.blend THEN st[o.dst][p] ELSE "-">>
                    ELSE st[o.dst][p]]]

RECURSIVE RunFrom(_, _, _)
RunFrom(st, b, i) == IF i > Len(b) THEN st ELSE RunFrom(Apply(st, b[i]), b, i + 1)
Run(st, b) == RunFrom(st, b, 1)

(* The rule. State while scanning: subresources (now) or textures (old rule) cleared whole,
   textures drawn into or filled or copied to, reads (texture, of a clear), and whether it
   already said no. *)
AllClear(cl, t) == IF NewRule THEN SubsOf(t) \subseteq cl ELSE t \in cl
Cleared(cl, s) == IF NewRule THEN s \in cl ELSE TexOf(s) \in cl

Step(r, o) ==
    IF r.no THEN r
    ELSE CASE o.k = "clear" -> [r EXCEPT !.cl = @ \cup {IF NewRule THEN o.dst ELSE TexOf(o.dst)}]
           [] o.k = "draw" ->
                IF ~Cleared(r.cl, o.dst) THEN [r EXCEPT !.no = TRUE]
                ELSE [r EXCEPT !.drawn = @ \cup {TexOf(o.dst)},
                               !.reads = IF o.src = 0 THEN @ ELSE @ \cup {<<o.src, AllClear(r.cl, o.src)>>}]
           [] o.k = "write" ->
                IF NewRule /\ <<TexOf(o.dst), FALSE>> \in r.reads THEN [r EXCEPT !.no = TRUE] ELSE r
           [] o.k = "copy" ->
                [r EXCEPT !.drawn = @ \cup {TexOf(o.dst)}, !.reads = @ \cup {<<TexOf(o.src), AllClear(r.cl, TexOf(o.src))>>}]
           [] o.k = "fill" -> [r EXCEPT !.drawn = @ \cup {TexOf(o.dst)}]

RECURSIVE ScanFrom(_, _, _)
ScanFrom(r, b, i) == IF i > Len(b) THEN r ELSE ScanFrom(Step(r, b[i]), b, i + 1)

ClearedTexs(cl) == IF NewRule THEN {TexOf(s) : s \in cl} ELSE cl

Repeats(b) ==
    LET r == ScanFrom([cl |-> {}, drawn |-> {}, reads |-> {}, no |-> FALSE], b, 1)
    IN /\ ~r.no
       /\ ~\E rd \in r.reads : ~rd[2] /\ (rd[1] \in r.drawn \/ (NewRule /\ rd[1] \in ClearedTexs(r.cl)))

VARIABLES batch, once, twice

Init == /\ batch \in Batches
        /\ once = Run(S0, batch)
        /\ twice = Run(once, batch)
Next == UNCHANGED <<batch, once, twice>>

\* (the invariant: a batch not run again leaves what running it would)
SkipIsSafe == Repeats(batch) => twice = once
\* (and the rule lets some batches be skipped at all)
SomeSkipped == ~Repeats(batch)
=============================================================================
