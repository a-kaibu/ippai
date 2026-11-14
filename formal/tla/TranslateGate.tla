---------------------------- MODULE TranslateGate ----------------------------
(* koji's translation gate (crates/koji/src/jit/mod.rs: GATE, Shared, Excl). Translations run
   on several threads at once; the 16-bit mode (SEG16, SEG16_ON) is one static every
   translation's decoder and generator read. A 32-bit translation (Shared) enters while no
   16-bit one waits or runs, counting itself; a 16-bit one (Excl) first marks WANT (one at a
   time), waits for the count to reach 0 and turns WANT into EXCL, sets the mode, translates,
   clears the mode and stores 0.

   Safety (NoMixedMode): a 32-bit translation never runs while the mode is 16-bit, and two
   16-bit ones never run at once.
   Liveness: a 16-bit translation that wants in gets in (readers drain, none start); a 32-bit
   one gets in (under strong fairness: 16-bit ones can come back to back).
   Gated = FALSE is the code as it was: no gate, the mode set and cleared around the 16-bit
   translation only (review b02: 32-bit code decoded with 16-bit rules). *)
EXTENDS Naturals, FiniteSets

CONSTANTS Readers, Writers, Gated

VARIABLES
  cnt,    \* GATE's count of 32-bit translations
  want,   \* GATE_WANT
  excl,   \* GATE_EXCL
  mode16, \* SEG16_ON
  pc

vars == <<cnt, want, excl, mode16, pc>>

Init == cnt = 0 /\ want = FALSE /\ excl = FALSE /\ mode16 = FALSE /\ pc = [p \in Readers \cup Writers |-> "idle"]

\* ---- a 32-bit translation ----

REnter(r) ==
  /\ pc[r] = "idle"
  /\ IF Gated THEN ~want /\ ~excl /\ cnt' = cnt + 1 ELSE UNCHANGED cnt
  /\ pc' = [pc EXCEPT ![r] = "in"]
  /\ UNCHANGED <<want, excl, mode16>>

\* (it reads the mode all through: one step per read)
RLeave(r) ==
  /\ pc[r] = "in"
  /\ IF Gated THEN cnt' = cnt - 1 ELSE UNCHANGED cnt
  /\ pc' = [pc EXCEPT ![r] = "idle"]
  /\ UNCHANGED <<want, excl, mode16>>

\* ---- a 16-bit translation ----

WWant(w) ==
  /\ pc[w] = "idle"
  /\ IF Gated THEN ~want /\ ~excl /\ want' = TRUE /\ pc' = [pc EXCEPT ![w] = "wait"]
     ELSE pc' = [pc EXCEPT ![w] = "set"] /\ UNCHANGED want
  /\ UNCHANGED <<cnt, excl, mode16>>

WExcl(w) ==
  /\ pc[w] = "wait"
  /\ cnt = 0 /\ want /\ ~excl
  /\ want' = FALSE /\ excl' = TRUE
  /\ pc' = [pc EXCEPT ![w] = "set"]
  /\ UNCHANGED <<cnt, mode16>>

WSet(w) ==
  /\ pc[w] = "set"
  /\ mode16' = TRUE
  /\ pc' = [pc EXCEPT ![w] = "in"]
  /\ UNCHANGED <<cnt, want, excl>>

WClear(w) ==
  /\ pc[w] = "in"
  /\ mode16' = FALSE
  /\ pc' = [pc EXCEPT ![w] = "out"]
  /\ UNCHANGED <<cnt, want, excl>>

\* (GATE.store(0))
WLeave(w) ==
  /\ pc[w] = "out"
  /\ IF Gated THEN excl' = FALSE /\ want' = FALSE /\ cnt' = 0 ELSE UNCHANGED <<excl, want, cnt>>
  /\ pc' = [pc EXCEPT ![w] = "idle"]
  /\ UNCHANGED mode16

Next ==
  \/ \E r \in Readers : REnter(r) \/ RLeave(r)
  \/ \E w \in Writers : WWant(w) \/ WExcl(w) \/ WSet(w) \/ WClear(w) \/ WLeave(w)

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A r \in Readers : WF_vars(RLeave(r)) /\ SF_vars(REnter(r))
  /\ \A w \in Writers : WF_vars(WExcl(w)) /\ WF_vars(WSet(w)) /\ WF_vars(WClear(w)) /\ WF_vars(WLeave(w)) /\ SF_vars(WWant(w))

TypeOK ==
  /\ cnt \in 0..Cardinality(Readers)
  /\ want \in BOOLEAN /\ excl \in BOOLEAN /\ mode16 \in BOOLEAN

NoMixedMode ==
  /\ \A r \in Readers : pc[r] = "in" => ~mode16
  /\ \A w1, w2 \in Writers : (w1 # w2 /\ pc[w1] \in {"set", "in", "out"}) => pc[w2] \notin {"set", "in", "out"}

\* (the count is the readers inside)
Counted == Gated => cnt = Cardinality({r \in Readers : pc[r] = "in"})

WriterGetsIn == \A w \in Writers : pc[w] = "wait" ~> pc[w] = "in"
ReaderGetsIn == \A r \in Readers : pc[r] = "idle" ~> pc[r] = "in"
=============================================================================
