--------------------------- MODULE ImageDecode ---------------------------
(* JPEG files the page decodes for the guest (crates/kura/src/idec.rs, web/src/idec.ts). A guest
   thread posts a file (`jpeg`: the request holds the file's bytes) and waits; the page lists the
   files not listed yet (vnhost_idec_list) and copies each out of the module's memory, decodes it,
   asks for room for the planes (vnhost_idec_begin: a buffer in the request), writes them and says
   it is done (vnhost_idec_done). The guest takes the planes (and frees the request) once done; a
   guest that stops waiting frees a request the page has not listed, and leaves a listed one to
   the page (abandoned: vnhost_idec_done frees it; vnhost_idec_begin gives it no room).

   Safety (Safe): the page never reads a freed file or writes freed planes, and a guest takes
   planes only when the page wrote all of them.
   Liveness: every request is freed in the end (with the page working).

   FreeListed = TRUE models a timeout that frees a listed request too: the page then copies the
   file, or writes the planes, of freed memory. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Guests, FreeListed

VARIABLES
  req,     \* per guest: its request (on: posted and not freed)
  live,    \* allocations not freed: <<g, "file">> and <<g, "px">>
  gs,      \* per guest: "idle", "waiting", "got", "fallback"
  page,    \* the page's step per guest's request: "", "copying", "decoding", "writing", "done"
  bad

vars == <<req, live, gs, page, bad>>

Empty == [on |-> FALSE, listed |-> FALSE, px |-> FALSE, written |-> FALSE, done |-> "", abandoned |-> FALSE]

Init == /\ req = [g \in Guests |-> Empty]
        /\ live = {}
        /\ gs = [g \in Guests |-> "idle"]
        /\ page = [g \in Guests |-> ""]
        /\ bad = FALSE

R(g) == req[g]

\* ---- guest ----

Post(g) ==
  /\ gs[g] = "idle"
  /\ req' = [req EXCEPT ![g] = [Empty EXCEPT !.on = TRUE]]
  /\ live' = live \cup {<<g, "file">>}
  /\ gs' = [gs EXCEPT ![g] = "waiting"]
  /\ UNCHANGED <<page, bad>>

\* `jpeg`: done, the planes taken (the request freed)
Take(g) ==
  /\ gs[g] = "waiting" /\ R(g).on /\ R(g).done /= ""
  /\ bad' = (bad \/ (R(g).done = "ok" /\ ~R(g).written))
  /\ gs' = [gs EXCEPT ![g] = IF R(g).done = "ok" THEN "got" ELSE "fallback"]
  /\ req' = [req EXCEPT ![g] = Empty]
  /\ live' = live \ {<<g, "file">>, <<g, "px">>}
  /\ UNCHANGED page

\* `jpeg`: late; a listed request is left to the page
Timeout(g) ==
  /\ gs[g] = "waiting" /\ R(g).on /\ R(g).done = ""
  /\ gs' = [gs EXCEPT ![g] = "fallback"]
  /\ IF R(g).listed /\ ~FreeListed
       THEN /\ req' = [req EXCEPT ![g].abandoned = TRUE]
            /\ UNCHANGED live
       ELSE /\ req' = [req EXCEPT ![g] = Empty]
            /\ live' = live \ {<<g, "file">>, <<g, "px">>}
  /\ UNCHANGED <<page, bad>>

\* ---- page ----

\* vnhost_idec_list, then the copy out of memory (its own step: the guest runs meanwhile)
List(g) ==
  /\ R(g).on /\ ~R(g).listed /\ page[g] = ""
  /\ req' = [req EXCEPT ![g].listed = TRUE]
  /\ page' = [page EXCEPT ![g] = "copying"]
  /\ UNCHANGED <<live, gs, bad>>

Copied(g) ==
  /\ page[g] = "copying"
  /\ bad' = (bad \/ <<g, "file">> \notin live)
  /\ page' = [page EXCEPT ![g] = "decoding"]
  /\ UNCHANGED <<req, live, gs>>

\* vnhost_idec_begin (room unless the guest left or the request is gone), then the planes written
Begin(g) ==
  /\ page[g] = "decoding"
  /\ IF R(g).on /\ R(g).done = "" /\ ~R(g).px /\ ~R(g).abandoned
       THEN /\ req' = [req EXCEPT ![g].px = TRUE]
            /\ live' = live \cup {<<g, "px">>}
            /\ page' = [page EXCEPT ![g] = "writing"]
       ELSE /\ page' = [page EXCEPT ![g] = "done"]
            /\ UNCHANGED <<req, live>>
  /\ UNCHANGED <<gs, bad>>

\* the decoder failed: vnhost_idec_done(id, 0) with no room asked
Failed(g) ==
  /\ page[g] = "decoding"
  /\ page' = [page EXCEPT ![g] = "done"]
  /\ UNCHANGED <<req, live, gs, bad>>

Written(g) ==
  /\ page[g] = "writing"
  /\ bad' = (bad \/ <<g, "px">> \notin live)
  /\ req' = IF R(g).on THEN [req EXCEPT ![g].written = TRUE] ELSE req
  /\ page' = [page EXCEPT ![g] = "done"]
  /\ UNCHANGED <<live, gs>>

\* vnhost_idec_done: frees an abandoned request, else says how it went
Done(g) ==
  /\ page[g] = "done"
  /\ IF R(g).on /\ R(g).abandoned
       THEN /\ req' = [req EXCEPT ![g] = Empty]
            /\ live' = live \ {<<g, "file">>, <<g, "px">>}
       ELSE IF R(g).on
         THEN /\ req' = [req EXCEPT ![g].done = IF R(g).px /\ R(g).written THEN "ok" ELSE "failed"]
              /\ UNCHANGED live
         ELSE UNCHANGED <<req, live>>
  /\ page' = [page EXCEPT ![g] = ""]
  /\ UNCHANGED <<gs, bad>>

Next == \E g \in Guests :
          \/ Post(g) \/ Take(g) \/ Timeout(g)
          \/ List(g) \/ Copied(g) \/ Begin(g) \/ Failed(g) \/ Written(g) \/ Done(g)

Spec == Init /\ [][Next]_vars
          /\ \A g \in Guests : WF_vars(List(g) \/ Copied(g) \/ Begin(g) \/ Written(g) \/ Done(g))
          /\ \A h \in Guests : WF_vars(Take(h))

Safe == ~bad

\* every request freed in the end, its memory too
Freed == <>[](\A g \in Guests : ~req[g].on /\ live = {})
=============================================================================
