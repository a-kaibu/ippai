---------------------------- MODULE ObjWake ----------------------------
(* WaitForSingleObject / WaitForMultipleObjects in sake (crates/sake/src/obj.rs over
   crates/waits/src/wake.rs, review P1-8): everything under the object table's lock, a
   condition variable per waiting thread. A release of `n` (ReleaseSemaphore; SetEvent of an
   auto-reset event and ReleaseMutex are 1) wakes every wait-all waiter of the object and the
   first `n` wait-any waiters of it that are not woken yet, oldest first. A waiter checks its
   objects in its list's order (wait-any takes the first one signaled, wait-all all of them or
   none); one that cannot goes back to sleep (its wake cleared: `rearm`); one that leaves hands
   the wakeup on: for each of its objects still signaled, one more waiter of it is woken
   (Table::leave). Others take objects without waiting (WaitForSingleObject(h, 0): Steal), and
   condition variables may wake a waiter without a notify (Spurious).

   loom checks the Rust with 2 waiters and 2 releasers (crates/waits/tests/loom.rs `objects`);
   this checks more of them at once: 4 waiters (wait-any and wait-all, lists in either order) on
   2 objects, any releases, steals and spurious wakes.

   Safe: tokens are never taken twice (counts stay >= 0, what was taken plus what is left is
   what was released). NoLostWake: an object that lets one through never has a waiter of it
   asleep and not woken while none of its waiters is awake (woken or checking): someone will
   look. NoHandoff = TRUE takes the hand-on out (a violation: the waiter woken for o1 takes o2,
   released meanwhile, and the waiter with only o1 sleeps on). *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS MaxReleases, MaxSteals, Spurious, NoHandoff

Objects == {"o1", "o2"}
Waiters == {"w1", "w2", "w3", "w4"}
\* each waiter's handle list (the order wait-any looks in) and whether it waits for all
List == [w1 |-> <<"o2", "o1">>, w2 |-> <<"o1">>, w3 |-> <<"o1", "o2">>, w4 |-> <<"o2">>]
All == [w1 |-> FALSE, w2 |-> FALSE, w3 |-> TRUE, w4 |-> FALSE]
Keys(w) == {List[w][i] : i \in 1..Len(List[w])}

VARIABLES
  count,     \* the objects' counts (a semaphore's count, an event's set)
  pc,        \* per waiter: "start" (calls the wait), "check" (looks, holding the lock), "sleep", "done"
  queue,     \* the wait list, oldest first (wake.rs WaitList::list)
  poked,     \* per waiter: woken since it last went to sleep
  released, stolen, took

vars == <<count, pc, queue, poked, released, stolen, took>>

Init ==
  /\ count = [o \in Objects |-> 0]
  /\ pc = [w \in Waiters |-> "start"]
  /\ queue = <<>>
  /\ poked = [w \in Waiters |-> FALSE]
  /\ released = 0 /\ stolen = 0
  /\ took = [w \in Waiters |-> {}]

In(w, q) == \E i \in 1..Len(q) : q[i] = w
InQueue(w) == In(w, queue)
\* the wait-any waiters of o not woken, oldest first
Asleep(o, p, q) == SelectSeq(q, LAMBDA w : o \in Keys(w) /\ ~All[w] /\ ~p[w])
\* WaitList::wake(o, n) on list q: every wait-all waiter of o, and the first n of Asleep
WakeQ(o, n, p, q) ==
  LET a == Asleep(o, p, q)
      first == {a[i] : i \in 1..(IF n < Len(a) THEN n ELSE Len(a))}
  IN [w \in Waiters |-> p[w] \/ (In(w, q) /\ o \in Keys(w) /\ All[w]) \/ w \in first]
WakeN(o, n, p) == WakeQ(o, n, p, queue)

Release(o, n) ==
  /\ released + n <= MaxReleases
  /\ count' = [count EXCEPT ![o] = @ + n]
  /\ released' = released + n
  /\ poked' = WakeN(o, n, poked)
  /\ UNCHANGED <<pc, queue, stolen, took>>

Steal(o) ==
  /\ stolen < MaxSteals /\ count[o] > 0
  /\ count' = [count EXCEPT ![o] = @ - 1]
  /\ stolen' = stolen + 1
  /\ UNCHANGED <<pc, queue, poked, released, took>>

Start(w) ==
  /\ pc[w] = "start"
  /\ pc' = [pc EXCEPT ![w] = "check"]
  /\ UNCHANGED <<count, queue, poked, released, stolen, took>>

\* the first object of w's list that is signaled (0: none)
First(w) == LET is == {i \in 1..Len(List[w]) : count[List[w][i]] > 0}
            IN IF is = {} THEN 0 ELSE CHOOSE i \in is : \A j \in is : i <= j
CanTake(w) == IF All[w] THEN \A o \in Keys(w) : count[o] > 0 ELSE First(w) # 0
Taken(w) == IF All[w] THEN Keys(w) ELSE {List[w][First(w)]}

\* takes what it waited for and leaves the list, handing the wakeup on (Table::leave)
Take(w) ==
  /\ pc[w] = "check" /\ CanTake(w)
  /\ LET t == Taken(w)
         c == [o \in Objects |-> IF o \in t THEN count[o] - 1 ELSE count[o]]
         q == SelectSeq(queue, LAMBDA x : x # w)
         p0 == [poked EXCEPT ![w] = FALSE]
         \* (one more waiter of each of its objects still signaled: o1 then o2)
         \* (after it left the list, as Table::leave removes it first)
         p1 == IF ~NoHandoff /\ "o1" \in Keys(w) /\ c["o1"] > 0 THEN WakeQ("o1", 1, p0, q) ELSE p0
         p2 == IF ~NoHandoff /\ "o2" \in Keys(w) /\ c["o2"] > 0 THEN WakeQ("o2", 1, p1, q) ELSE p1
     IN /\ count' = c
        /\ took' = [took EXCEPT ![w] = t]
        /\ queue' = q
        /\ poked' = p2
  /\ pc' = [pc EXCEPT ![w] = "done"]
  /\ UNCHANGED <<released, stolen>>

\* nothing to take: into the list (the first time), wake cleared, asleep
Sleep(w) ==
  /\ pc[w] = "check" /\ ~CanTake(w)
  /\ queue' = IF InQueue(w) THEN queue ELSE Append(queue, w)
  /\ poked' = [poked EXCEPT ![w] = FALSE]
  /\ pc' = [pc EXCEPT ![w] = "sleep"]
  /\ UNCHANGED <<count, released, stolen, took>>

Wake(w) ==
  /\ pc[w] = "sleep" /\ (poked[w] \/ Spurious)
  /\ pc' = [pc EXCEPT ![w] = "check"]
  /\ UNCHANGED <<count, queue, poked, released, stolen, took>>

Next ==
  \/ \E o \in Objects, n \in 1..2 : Release(o, n)
  \/ \E o \in Objects : Steal(o)
  \/ \E w \in Waiters : Start(w) \/ Take(w) \/ Sleep(w) \/ Wake(w)

Spec == Init /\ [][Next]_vars

TypeOK == /\ \A o \in Objects : count[o] \in Nat
          /\ \A w \in Waiters : pc[w] \in {"start", "check", "sleep", "done"}

Safe ==
  /\ TypeOK
  /\ LET takes == {<<w, o>> \in Waiters \X Objects : o \in took[w]}
     IN Cardinality(takes) + stolen + count["o1"] + count["o2"] = released

Awake(w) == pc[w] = "check" \/ (pc[w] = "sleep" /\ poked[w])
NoLostWake ==
  \A o \in Objects : count[o] > 0 =>
    /\ (\E w \in Waiters : pc[w] = "sleep" /\ ~poked[w] /\ ~All[w] /\ o \in Keys(w))
         => \E w \in Waiters : Awake(w) /\ o \in Keys(w)
    \* (a wait-all waiter that could take everything is never left asleep unwoken)
    /\ \A w \in Waiters : (pc[w] = "sleep" /\ All[w] /\ o \in Keys(w) /\ \A k \in Keys(w) : count[k] > 0) => poked[w]
=============================================================================
