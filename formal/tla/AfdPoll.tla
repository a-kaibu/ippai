---- MODULE AfdPoll ----
\* sake afd.rs: an IOCTL_AFD_POLL that finds no asked event yet is queued (POLLS) and ended by
\* afd::check() later. Nothing polls on a timer for a waiter on the poll's event, so the poll
\* must not wait while its socket is ready (review b04: "a socket that becomes ready between
\* fired() and push is only noticed by the next check()").
\*   Hook:    ws2 calls check() after a socket changes (ws2::with, dirty)
\*   Recheck: ioctl calls check() after queueing the poll
\* Both are needed: AfdPollNoHook.cfg and AfdPollNoRecheck.cfg violate NoLostWakeup.
CONSTANTS Hook, Recheck
VARIABLES ready, poll, pcP, pcS, saw

vars == <<ready, poll, pcP, pcS, saw>>

Init == ready = FALSE /\ poll = "none" /\ pcP = "look" /\ pcS = "change" /\ saw = FALSE

\* check(): under the POLLS lock, ends the queued poll whose socket is ready
Check == IF poll = "waiting" /\ ready THEN poll' = "done" ELSE UNCHANGED poll

\* the poller (ioctl)
Look == pcP = "look" /\ saw' = ready /\ pcP' = "queue" /\ UNCHANGED <<ready, poll, pcS>>
Queue == pcP = "queue" /\
    (IF saw THEN poll' = "done" /\ pcP' = "end"
     ELSE poll' = "waiting" /\ pcP' = (IF Recheck THEN "recheck" ELSE "end"))
    /\ UNCHANGED <<ready, pcS, saw>>
Again == pcP = "recheck" /\ Check /\ pcP' = "end" /\ UNCHANGED <<ready, pcS, saw>>

\* the socket (data arrives, through ws2::with)
Change == pcS = "change" /\ ready' = TRUE /\ pcS' = (IF Hook THEN "hook" ELSE "end") /\ UNCHANGED <<poll, pcP, saw>>
Hooked == pcS = "hook" /\ Check /\ pcS' = "end" /\ UNCHANGED <<ready, pcP, saw>>

Done == pcP = "end" /\ pcS = "end" /\ UNCHANGED vars

Next == Look \/ Queue \/ Again \/ Change \/ Hooked \/ Done

Spec == Init /\ [][Next]_vars

\* once both are through, no poll waits on a ready socket (nobody would end it)
NoLostWakeup == (pcP = "end" /\ pcS = "end") => ~(poll = "waiting" /\ ready)
====
