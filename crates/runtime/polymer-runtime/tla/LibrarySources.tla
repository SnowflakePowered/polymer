--------------------------- MODULE LibrarySources ---------------------------
(*
  Library sources: watched folders reconciled into the game catalog.

  Disk: one ROM file ("rom", versioned, may be partially written) and one
  title directory ("dir") whose marker file (meta.xml / PARAM.SFO) decides
  whether it is an in-place title. Without the marker, the inner file
  ("inner", e.g. USRDIR/EBOOT.BIN) is identified as a standalone unit.

  Catalog keys are import units: "rom", "dir", "inner". Every watcher event,
  scan step, manual import, and source removal requests a reconcile of a key.
  A reconcile reads the disk (identify) and later commits to the catalog in
  a single writer transaction.

  Design switches (negative controls flip one at a time):
    WatchFirst        the watcher starts before the initial scan walks the tree
    RescanOnOverflow  a dropped event batch requests a full rescan
    OfflineIsAbsent   an unreachable root reads as "file absent"
    Dedupe            "rerun": a request during an in-flight reconcile reruns it
                      "swallow": deduped against the active job (JobStore today)
                      "none": no per-key exclusion between workers
    OwnerPolicy       "derived": ownership = coverage by an active source,
                      computed at commit; "single": launchables.source_id is
                      the requesting source, removal deletes owned rows
    BoundaryRescan    a marker change also reconciles keys under the directory
*)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS Sources, Workers, Covers, MaxEnv, MaxOverflow, MaxVer, MaxManual,
          WatchFirst, RescanOnOverflow, OfflineIsAbsent, Dedupe, OwnerPolicy,
          BoundaryRescan

Keys == {"rom", "dir", "inner"}
Files == {"rom", "marker", "inner"}
Origins == Sources \cup {"manual"}
Order == <<"dir", "inner", "rom">>
Contents == [v : 0..MaxVer, done : BOOLEAN]
Fixed == [v |-> 0, done |-> TRUE]
Obs == [k : {"unit", "absent", "error"}, c : Contents]
Gone == [here |-> FALSE, c |-> Fixed, manual |-> FALSE, owner |-> "none"]
Entries == [here : BOOLEAN, c : Contents, manual : BOOLEAN, owner : Sources \cup {"none"}]

VARIABLES romHere, romVer, romDone, marker, inner, online,
          reg, watching, evq, scanPc, scanReq,
          dirty, wpc, wx, worig, wobs, wfalse,
          entry, env, overflows, manuals, falseDeletes, lostManual

disk == <<romHere, romVer, romDone, marker, inner, online>>
src == <<reg, watching, evq, scanPc, scanReq>>
work == <<dirty, wpc, wx, worig, wobs, wfalse>>
ghost == <<env, overflows, manuals, falseDeletes, lostManual>>
vars == <<disk, src, work, entry, ghost>>

FileCovers(s) == (Covers[s] \cap {"rom", "inner"})
                 \cup (IF "dir" \in Covers[s] THEN {"marker"} ELSE {})

IsUnit(x) == CASE x = "rom" -> romHere
             []   x = "dir" -> marker
             []   x = "inner" -> inner /\ ~marker

Content(x) == IF x = "rom" THEN [v |-> romVer, done |-> romDone] ELSE Fixed

Observe(x) ==
  IF ~online
    THEN [k |-> IF OfflineIsAbsent THEN "absent" ELSE "error", c |-> Fixed]
    ELSE IF IsUnit(x) THEN [k |-> "unit", c |-> Content(x)]
    ELSE [k |-> "absent", c |-> Fixed]

Covered(x) == \E s \in Sources : reg[s] = "active" /\ x \in Covers[s]

Inflight == {wx[w] : w \in {v \in Workers : wpc[v] = "commit"}}

Targets(f) == CASE f = "rom" -> {"rom"}
              []   f = "inner" -> {"inner"}
              []   f = "marker" -> IF BoundaryRescan THEN {"dir", "inner"} ELSE {"dir"}

TypeOK ==
  /\ romHere \in BOOLEAN /\ romVer \in 0..MaxVer /\ romDone \in BOOLEAN
  /\ marker \in BOOLEAN /\ inner \in BOOLEAN /\ online \in BOOLEAN
  /\ reg \in [Sources -> {"unused", "active", "removed"}]
  /\ watching \in [Sources -> BOOLEAN]
  /\ evq \in [Sources -> SUBSET Files]
  /\ scanPc \in [Sources -> 0..Len(Order)]
  /\ scanReq \in [Sources -> BOOLEAN]
  /\ dirty \in [Keys -> SUBSET Origins]
  /\ wpc \in [Workers -> {"idle", "commit"}]
  /\ wx \in [Workers -> Keys]
  /\ worig \in [Workers -> SUBSET Origins]
  /\ wobs \in [Workers -> Obs]
  /\ wfalse \in [Workers -> BOOLEAN]
  /\ entry \in [Keys -> Entries]

Init ==
  /\ romHere = TRUE /\ romVer = 1 /\ romDone = TRUE
  /\ marker = FALSE /\ inner = TRUE /\ online = TRUE
  /\ reg = [s \in Sources |-> "unused"]
  /\ watching = [s \in Sources |-> FALSE]
  /\ evq = [s \in Sources |-> {}]
  /\ scanPc = [s \in Sources |-> 0]
  /\ scanReq = [s \in Sources |-> FALSE]
  /\ dirty = [x \in Keys |-> {}]
  /\ wpc = [w \in Workers |-> "idle"]
  /\ wx = [w \in Workers |-> "rom"]
  /\ worig = [w \in Workers |-> {}]
  /\ wobs = [w \in Workers |-> [k |-> "absent", c |-> Fixed]]
  /\ wfalse = [w \in Workers |-> FALSE]
  /\ entry = [x \in Keys |-> Gone]
  /\ env = 0 /\ overflows = 0 /\ manuals = 0 /\ falseDeletes = 0 /\ lostManual = FALSE

\* A request made while the key is being reconciled is dropped under "swallow".
Request(X, o) ==
  dirty' = [x \in Keys |->
              IF x \in X /\ ~(Dedupe = "swallow" /\ x \in Inflight)
                THEN dirty[x] \cup {o} ELSE dirty[x]]

Emit(F) ==
  evq' = [s \in Sources |-> IF watching[s] THEN evq[s] \cup (F \cap FileCovers(s)) ELSE evq[s]]

EnvStep == env < MaxEnv /\ env' = env + 1

-----------------------------------------------------------------------------
\* Environment: the user's files and drive.

RomWrite ==
  /\ EnvStep /\ romVer < MaxVer
  /\ romHere' = TRUE /\ romVer' = romVer + 1 /\ romDone' = FALSE
  /\ Emit({"rom"})
  /\ UNCHANGED <<marker, inner, online, reg, watching, scanPc, scanReq, work, entry,
                 overflows, manuals, falseDeletes, lostManual>>

RomFinish ==
  /\ romHere /\ ~romDone
  /\ romDone' = TRUE
  /\ Emit({"rom"})
  /\ UNCHANGED <<romHere, romVer, marker, inner, online, reg, watching, scanPc, scanReq,
                 work, entry, ghost>>

RomDelete ==
  /\ EnvStep /\ romHere
  /\ romHere' = FALSE /\ romDone' = TRUE
  /\ Emit({"rom"})
  /\ UNCHANGED <<romVer, marker, inner, online, reg, watching, scanPc, scanReq, work, entry,
                 overflows, manuals, falseDeletes, lostManual>>

MarkerToggle ==
  /\ EnvStep
  /\ marker' = ~marker
  /\ Emit({"marker"})
  /\ UNCHANGED <<romHere, romVer, romDone, inner, online, reg, watching, scanPc, scanReq,
                 work, entry, overflows, manuals, falseDeletes, lostManual>>

InnerToggle ==
  /\ EnvStep
  /\ inner' = ~inner
  /\ Emit({"inner"})
  /\ UNCHANGED <<romHere, romVer, romDone, marker, online, reg, watching, scanPc, scanReq,
                 work, entry, overflows, manuals, falseDeletes, lostManual>>

GoOffline ==
  /\ EnvStep /\ online
  /\ online' = FALSE
  /\ watching' = [s \in Sources |-> FALSE]
  /\ evq' = [s \in Sources |-> {}]
  /\ UNCHANGED <<romHere, romVer, romDone, marker, inner, reg, scanPc, scanReq, work, entry,
                 overflows, manuals, falseDeletes, lostManual>>

GoOnline ==
  /\ EnvStep /\ ~online
  /\ online' = TRUE
  /\ watching' = [s \in Sources |-> reg[s] = "active" /\ WatchFirst]
  /\ scanReq' = [s \in Sources |-> scanReq[s] \/ reg[s] = "active"]
  /\ UNCHANGED <<romHere, romVer, romDone, marker, inner, reg, evq, scanPc, work, entry,
                 overflows, manuals, falseDeletes, lostManual>>

Overflow(s) ==
  /\ overflows < MaxOverflow /\ evq[s] # {}
  /\ overflows' = overflows + 1
  /\ evq' = [evq EXCEPT ![s] = {}]
  /\ scanReq' = [scanReq EXCEPT ![s] = @ \/ RescanOnOverflow]
  /\ UNCHANGED <<disk, reg, watching, scanPc, work, entry, env, manuals, falseDeletes, lostManual>>

-----------------------------------------------------------------------------
\* Theme-driven source management and drag-and-drop imports.

AddSource(s) ==
  /\ reg[s] = "unused"
  /\ reg' = [reg EXCEPT ![s] = "active"]
  /\ scanReq' = [scanReq EXCEPT ![s] = TRUE]
  /\ watching' = [watching EXCEPT ![s] = WatchFirst /\ online]
  /\ UNCHANGED <<disk, evq, scanPc, work, entry, ghost>>

RemoveSource(s) ==
  /\ reg[s] = "active"
  /\ reg' = [reg EXCEPT ![s] = "removed"]
  /\ watching' = [watching EXCEPT ![s] = FALSE]
  /\ evq' = [evq EXCEPT ![s] = {}]
  /\ scanPc' = [scanPc EXCEPT ![s] = 0]
  /\ scanReq' = [scanReq EXCEPT ![s] = FALSE]
  /\ IF OwnerPolicy = "derived"
       THEN /\ entry' = [x \in Keys |->
                          IF x \in Covers[s] /\ ~entry[x].manual
                             /\ ~\E t \in Sources \ {s} : reg[t] = "active" /\ x \in Covers[t]
                            THEN Gone ELSE entry[x]]
            /\ UNCHANGED dirty
       ELSE /\ entry' = [x \in Keys |->
                          IF entry[x].here /\ entry[x].owner = s /\ ~entry[x].manual
                            THEN Gone ELSE entry[x]]
            /\ UNCHANGED dirty
  /\ UNCHANGED <<disk, wpc, wx, worig, wobs, wfalse, ghost>>

ManualImport(x) ==
  /\ manuals < MaxManual
  /\ manuals' = manuals + 1
  /\ Request({x}, "manual")
  /\ UNCHANGED <<disk, src, wpc, wx, worig, wobs, wfalse, entry, env, overflows,
                 falseDeletes, lostManual>>

-----------------------------------------------------------------------------
\* System: watcher delivery, scans, reconcile workers.

Deliver(s, f) ==
  /\ f \in evq[s]
  /\ evq' = [evq EXCEPT ![s] = @ \ {f}]
  /\ Request(Targets(f), s)
  /\ UNCHANGED <<disk, reg, watching, scanPc, scanReq, wpc, wx, worig, wobs, wfalse, entry, ghost>>

StartScan(s) ==
  /\ reg[s] = "active" /\ scanReq[s] /\ scanPc[s] = 0 /\ online
  /\ scanPc' = [scanPc EXCEPT ![s] = 1]
  /\ scanReq' = [scanReq EXCEPT ![s] = FALSE]
  /\ UNCHANGED <<disk, reg, watching, evq, work, entry, ghost>>

\* The walk and the catalog rows under the root both feed reconcile requests.
ScanStep(s) ==
  /\ scanPc[s] > 0
  /\ Request({Order[scanPc[s]]} \cap Covers[s], s)
  /\ scanPc' = [scanPc EXCEPT ![s] = IF @ = Len(Order) THEN 0 ELSE @ + 1]
  /\ watching' = [watching EXCEPT ![s] =
                    @ \/ (scanPc[s] = Len(Order) /\ ~WatchFirst /\ online)]
  /\ UNCHANGED <<disk, reg, evq, scanReq, wpc, wx, worig, wobs, wfalse, entry, ghost>>

Take(w, x) ==
  /\ wpc[w] = "idle" /\ dirty[x] # {}
  /\ Dedupe = "none" \/ x \notin Inflight
  /\ wpc' = [wpc EXCEPT ![w] = "commit"]
  /\ wx' = [wx EXCEPT ![w] = x]
  /\ worig' = [worig EXCEPT ![w] = dirty[x]]
  /\ wobs' = [wobs EXCEPT ![w] = Observe(x)]
  /\ wfalse' = [wfalse EXCEPT ![w] = ~online /\ IsUnit(x)]
  /\ dirty' = [dirty EXCEPT ![x] = {}]
  /\ UNCHANGED <<disk, src, entry, ghost>>

Manual(x, orig, obs) ==
  (entry[x].here /\ entry[x].manual) \/ ("manual" \in orig /\ obs.k = "unit")

Derived(x, orig, obs) ==
  IF ~Covered(x) /\ ~Manual(x, orig, obs) THEN Gone
  ELSE IF obs.k = "error" THEN entry[x]
  ELSE IF obs.k = "absent"
    THEN IF Manual(x, orig, obs) THEN entry[x] ELSE Gone
  ELSE IF Covered(x) \/ Manual(x, orig, obs)
    THEN [here |-> TRUE, c |-> obs.c, manual |-> Manual(x, orig, obs), owner |-> "none"]
  ELSE Gone

Single(x, orig, obs, o) ==
  IF obs.k = "error" THEN entry[x]
  ELSE IF obs.k = "absent"
    THEN IF Manual(x, orig, obs) THEN entry[x] ELSE Gone
  ELSE [here |-> TRUE, c |-> obs.c, manual |-> Manual(x, orig, obs), owner |-> o]

SingleOwners(x, orig) ==
  IF orig \cap Sources # {} THEN orig \cap Sources ELSE {entry[x].owner}

Commit(w) ==
  LET x == wx[w]
      orig == worig[w]
      obs == wobs[w]
  IN
  /\ wpc[w] = "commit"
  /\ wpc' = [wpc EXCEPT ![w] = "idle"]
  /\ \E o \in SingleOwners(x, orig) :
       entry' = [entry EXCEPT ![x] =
                   IF OwnerPolicy = "derived" THEN Derived(x, orig, obs)
                   ELSE Single(x, orig, obs, o)]
  /\ falseDeletes' = falseDeletes
       + (IF wfalse[w] /\ Covered(x) /\ entry[x].here /\ ~entry'[x].here THEN 1 ELSE 0)
  /\ lostManual' = (lostManual \/ (entry[x].here /\ entry[x].manual /\ ~entry'[x].here))
  /\ UNCHANGED <<disk, src, dirty, wx, worig, wobs, wfalse, env, overflows, manuals>>

-----------------------------------------------------------------------------

Env ==
  \/ RomWrite \/ RomFinish \/ RomDelete \/ MarkerToggle \/ InnerToggle
  \/ GoOffline \/ GoOnline
  \/ \E s \in Sources : Overflow(s) \/ AddSource(s) \/ RemoveSource(s)
  \/ \E x \in Keys : ManualImport(x)

System ==
  \/ \E s \in Sources : StartScan(s) \/ ScanStep(s) \/ \E f \in Files : Deliver(s, f)
  \/ \E w \in Workers : Commit(w) \/ \E x \in Keys : Take(w, x)

Next == Env \/ System

Fairness ==
  /\ WF_vars(RomFinish)
  /\ \A s \in Sources : WF_vars(StartScan(s)) /\ WF_vars(ScanStep(s))
  /\ \A s \in Sources : \A f \in Files : WF_vars(Deliver(s, f))
  /\ \A w \in Workers : WF_vars(Commit(w)) /\ \A x \in Keys : WF_vars(Take(w, x))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* Properties.

\* An unreachable root never deletes a covered launchable that still exists on disk.
NoFalseDelete == falseDeletes = 0

\* Drag-and-drop imports belong to no source and are never removed by source management.
ManualKept == ~lostManual

\* No row points at a removed source.
NoOrphanOwner ==
  \A x \in Keys : entry[x].here /\ entry[x].owner \in Sources => reg[entry[x].owner] = "active"

Correct(x) ==
  LET e == entry[x] IN
  IF IsUnit(x)
    THEN /\ Covered(x) => e.here /\ e.c = Content(x)
         /\ e.here => Covered(x) \/ e.manual
    ELSE e.here => e.manual

\* Once the environment settles on a reachable drive, the catalog matches the disk.
Converges == <>[](online => \A x \in Keys : Correct(x))

=============================================================================
