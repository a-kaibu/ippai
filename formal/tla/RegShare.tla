----------------------------- MODULE RegShare -----------------------------
(* sake's registry shared by processes (crates/sake/src/misc.rs: with_reg_eager, reg_sync,
   merge_reg, reg_batch). Each process keeps the registry in memory; the file
   (C:\users\user\sake-registry.tsv) is what the others see. A transaction runs under the
   kernel's lock (kern::with): it first takes in what the others wrote since it last looked
   (reg_sync: a cell this process changed since `base` stays its own, every other cell becomes
   the file's), then makes its change, then writes the file at once, unless the process is in
   a batch (msi's standard actions), which writes once at its end.

   Cells stand for values (the key-level rules, made and deleted keys, are checked by the
   proptest two_processes_lose_no_change on the real merge_reg).

   Safety:
   - NoLostChange: the file is what the processes' writes meant, in the order they wrote:
     each cell holds the last change (a set to another value than the process saw then) of
     the process that wrote last having changed it (`want`, from a ghost of each one's own
     changes `mine`). A batch's changes count when it is written: another process's change
     to the same cell made meanwhile gives way to it, as if the batch had run at its end.
   - OutsideBatchWritten: a process outside a batch has nothing waiting.
   - OwnChangesSeen: a process sees its own last set of a cell until another sets it.
   Mode = "merge" is the code. "old" is the reload as it was (the file's keys replace ours:
   what a batch set is lost when another process writes); "overwrite" writes without taking
   in the others' (theirs lost). Both must fail. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Procs, Cells, Vals, Mode, MaxSteps

None == 99

VARIABLES
  file,    \* the file's cells
  m,       \* each process's cells
  base,    \* each process's file as it last read or wrote it
  batch,   \* each process in a reg_batch
  mine,    \* ghost: each process's own changes not written yet (None: none): its last set of
           \* a cell when that differs from what the file holds
  want,    \* ghost: each cell's value as set by the last writer that set it
  last,    \* ghost: each cell's last set of all, in time (OwnChangesSeen)
  lastBy,  \* ghost: who made it (0: nobody yet)
  steps

vars == <<file, m, base, batch, mine, want, last, lastBy, steps>>

Init ==
  /\ file = [c \in Cells |-> 0]
  /\ m = [p \in Procs |-> file]
  /\ base = [p \in Procs |-> file]
  /\ batch = [p \in Procs |-> FALSE]
  /\ mine = [p \in Procs |-> [c \in Cells |-> None]]
  /\ want = file
  /\ last = file
  /\ lastBy = [c \in Cells |-> 0]
  /\ steps = 0

\* reg_sync: the cells after taking in the file
Synced(p) ==
  CASE Mode = "merge" -> [c \in Cells |-> IF m[p][c] # base[p][c] THEN m[p][c] ELSE file[c]]
    [] Mode = "old" -> file
    [] Mode = "overwrite" -> m[p]

\* writing cells `cs` of process p, whose own sets not written yet are `ms`: the file, and the
\* ghost of what was meant
Wrote(p, cs, ms) ==
  /\ file' = cs
  /\ base' = [base EXCEPT ![p] = cs]
  /\ want' = [c \in Cells |-> IF ms[c] # None THEN ms[c] ELSE want[c]]
  /\ mine' = [mine EXCEPT ![p] = [c \in Cells |-> None]]

\* the ghost of a process's own changes once it read the file again: a change the file now
\* holds anyway is no change of its own
Kept(p) == [c \in Cells |-> IF mine[p][c] = file[c] THEN None ELSE mine[p][c]]

\* a transaction setting cell c to v (RegSetValueEx, reg_put)
Set(p, c, v) ==
  /\ steps < MaxSteps
  /\ LET cs == [Synced(p) EXCEPT ![c] = v]
         \* (a set back to what it read of the file is no change of its own)
         ms == [Kept(p) EXCEPT ![c] = IF v # file[c] THEN v ELSE None] IN
       /\ m' = [m EXCEPT ![p] = cs]
       /\ IF batch[p]
            THEN /\ base' = [base EXCEPT ![p] = IF Mode = "overwrite" THEN base[p] ELSE file]
                 /\ mine' = [mine EXCEPT ![p] = ms]
                 /\ UNCHANGED <<file, want>>
            ELSE Wrote(p, cs, ms)
  /\ batch' = batch
  /\ last' = [last EXCEPT ![c] = v]
  /\ lastBy' = [lastBy EXCEPT ![c] = p]
  /\ steps' = steps + 1

\* a transaction that only reads (RegQueryValueEx): takes in the others' cells
Look(p) ==
  /\ steps < MaxSteps
  /\ m' = [m EXCEPT ![p] = Synced(p)]
  /\ base' = [base EXCEPT ![p] = IF Mode = "overwrite" THEN base[p] ELSE file]
  /\ mine' = [mine EXCEPT ![p] = Kept(p)]
  /\ UNCHANGED <<file, batch, want, last, lastBy>>
  /\ steps' = steps + 1

BatchStart(p) ==
  /\ ~batch[p] /\ steps < MaxSteps
  /\ batch' = [batch EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<file, m, base, mine, want, last, lastBy>>
  /\ steps' = steps + 1

\* reg_batch's end: reg_flush, a transaction written now
BatchEnd(p) ==
  /\ batch[p]
  /\ batch' = [batch EXCEPT ![p] = FALSE]
  /\ m' = [m EXCEPT ![p] = Synced(p)]
  /\ Wrote(p, Synced(p), Kept(p))
  /\ UNCHANGED <<last, lastBy>>
  /\ steps' = steps + 1

Next == \E p \in Procs :
          \/ \E c \in Cells, v \in Vals : Set(p, c, v)
          \/ Look(p) \/ BatchStart(p) \/ BatchEnd(p)

Spec == Init /\ [][Next]_vars

NoLostChange == file = want
OutsideBatchWritten == \A p \in Procs : ~batch[p] => \A c \in Cells : mine[p][c] = None
OwnChangesSeen == \A p \in Procs, c \in Cells : mine[p][c] # None /\ lastBy[c] = p => m[p][c] = mine[p][c]
=============================================================================
