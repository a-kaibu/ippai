----------------------------- MODULE WmStart -----------------------------
(* The async WM reader's delivery thread against the program's Start and Stop (crates/sake/
   src/wmvcore/asyncr.rs: start, Async::seeking / read / step, thread's Step::Read and
   Step::Wait, sleep_from / wake).

   The file is a position (3 objects a generation): the seek of a Start puts it at the new start's key frame (pos 0 of
   a new "epoch" of positions), each read takes the next. The thread reads under the reader
   lock, the same lock Start's seek takes; the Async state (running, held, epoch) is the table
   lock's. The thread sleeps only while the wake count is the one it saw before it looked
   (no lost wakeup).

   Safety (FromTheSeek): a sample held after Start's seek is from after that seek, and the
   first one is the key frame. Liveness (Delivers): a Start that stays is followed by its key
   frame being handed out.
   Old = TRUE is sake as it was: Start seeked, then cleared `held`, with no epoch: a read the
   thread began before the seek could land after (a stale sample), or a read of the key frame
   made between the seek and the clear was thrown away (the decoder started on deltas). *)
EXTENDS Naturals

CONSTANTS Old, MaxStarts

VARIABLES
  seekto,    \* the file's current seek generation (Start's count)
  filepos,   \* the next position read in that generation
  running, held, epoch,  \* the Async state (held: <<gen, pos>> or <<>>)
  delivered, \* <<gen, pos>> handed out last, or <<>>
  wakes,     \* the wake count
  tpc, te, tgot, tseen, \* thread: pc, epoch of its read, what it read, wake count seen
  ppc, starts \* program: pc of a Start in progress, Starts made

vars == <<seekto, filepos, running, held, epoch, delivered, wakes, tpc, te, tgot, tseen, ppc, starts>>

Init ==
  /\ seekto = 0 /\ filepos = 0 /\ running = TRUE /\ held = <<>> /\ epoch = 0
  /\ delivered = <<>> /\ wakes = 0
  /\ tpc = "look" /\ te = 0 /\ tgot = <<>> /\ tseen = 0
  /\ ppc = "idle" /\ starts = 0

\* ---- the delivery thread
TLook == tpc = "look" /\ tseen' = wakes /\
  IF held # <<>> THEN /\ delivered' = held /\ held' = <<>>
                      /\ UNCHANGED <<tpc, te, tgot, seekto, filepos, running, epoch, wakes, ppc, starts>>
  ELSE IF running /\ filepos < 3 THEN /\ tpc' = "lock" /\ te' = epoch
                       /\ UNCHANGED <<held, delivered, tgot, seekto, filepos, running, epoch, wakes, ppc, starts>>
  ELSE /\ tpc' = "sleep" /\ UNCHANGED <<held, delivered, te, tgot, seekto, filepos, running, epoch, wakes, ppc, starts>>

\* the reader lock taken (not while Start's seek holds it); the epoch checked again under it
TRead == tpc = "lock" /\
  (IF ~Old /\ epoch # te THEN /\ tpc' = "look" /\ UNCHANGED <<tgot, filepos>>
   ELSE /\ tgot' = <<seekto, filepos>> /\ filepos' = filepos + 1 /\ tpc' = "store")
  /\ UNCHANGED <<seekto, running, held, epoch, delivered, wakes, tseen, te, ppc, starts>>

TStore == tpc = "store" /\ tpc' = "look" /\
  (IF Old THEN held' = tgot
   ELSE IF te = epoch /\ running THEN held' = tgot ELSE held' = held)
  /\ UNCHANGED <<seekto, filepos, running, epoch, delivered, wakes, tseen, te, tgot, ppc, starts>>

\* sleeps only if no wake came since it looked
TSleep == tpc = "sleep" /\ wakes # tseen /\ tpc' = "look"
  /\ UNCHANGED <<seekto, filepos, running, held, epoch, delivered, wakes, tseen, te, tgot, ppc, starts>>

\* ---- the program's Start(t): seeking (new), the seek under the reader lock, a.start + wake
PBegin == ppc = "idle" /\ starts < MaxStarts /\ starts' = starts + 1 /\ ppc' = "seek" /\
  (IF Old THEN UNCHANGED <<running, held, epoch>>
   ELSE /\ running' = FALSE /\ held' = <<>> /\ epoch' = epoch + 1)
  /\ UNCHANGED <<seekto, filepos, delivered, wakes, tpc, te, tgot, tseen>>
\* (the thread's read and the seek are each one step under the reader lock)
PSeek == ppc = "seek" /\ seekto' = seekto + 1 /\ filepos' = 0 /\ ppc' = "start"
  /\ UNCHANGED <<running, held, epoch, delivered, wakes, tpc, te, tgot, tseen, starts>>
PStart == ppc = "start" /\ ppc' = "idle" /\ running' = TRUE /\ held' = <<>> /\ wakes' = wakes + 1
  /\ UNCHANGED <<seekto, filepos, epoch, delivered, tpc, te, tgot, tseen, starts>>

Next == TLook \/ TRead \/ TStore \/ TSleep \/ PBegin \/ PSeek \/ PStart

Spec == Init /\ [][Next]_vars /\ WF_vars(TLook \/ TRead \/ TStore \/ TSleep) /\ WF_vars(PSeek \/ PStart)

\* once Start is done, what is held or handed out is of its seek, and in order from its key frame
FromTheSeek == ppc = "idle" /\ starts > 0 /\ held # <<>> => held[1] = seekto

\* after the last Start, its key frame (pos 0) is handed out
Delivers == <>[](starts = MaxStarts /\ ppc = "idle") => <>(delivered = <<seekto, 0>>)
=============================================================================
