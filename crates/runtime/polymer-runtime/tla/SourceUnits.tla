----------------------------- MODULE SourceUnits -----------------------------
(*
  How a source divides a folder into units while markers change, folders move,
  and the drive disappears mid-reconcile.

  One title folder lives at "A" or "B" (renamed between them) or is gone. It
  holds a marker file (PARAM.SFO, meta/meta.xml) that is absent, valid, or
  invalid (present but not a title, e.g. a PS3 patch or an encrypted Vita
  dump), and an inner file that is a standalone unit only while the folder is
  not a title. Keys per location: the folder, its marker, and its inner file.

  A reconcile takes a key, stats it, then probes the disk (identify, locate),
  then commits. Requests made while a key is in flight rerun it.

  Switches (negative controls flip one):
    MarkerRule      "always": a marker change always requests its folder;
                    "kind": only when the folder's catalog kind differs from disk
    ExpandMarkers   expanding a folder that is not a unit also requests its
                    marker file, which is never a unit itself
    MarkerOnDelete  a missing marker is still followed to its folder
    KeepRenames     the watcher keeps a renamed folder's destination
    ProbeError      a probe on an unreachable drive is an error, not "no unit"
*)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS MaxEnv, MarkerRule, ExpandMarkers, MarkerOnDelete, KeepRenames, ProbeError

Places == {"A", "B"}
Kinds == {"dir", "marker", "inner"}
Keys == Places \X Kinds
Absent == "none"

VARIABLES loc, marker, inner, online, watching, events, dirty, pc, key, seen,
          row, env, falseDeletes

vars == <<loc, marker, inner, online, watching, events, dirty, pc, key, seen, row, env,
          falseDeletes>>

Title(p) == loc = p /\ marker = "valid"
Unit(k) == CASE k[2] = "dir" -> Title(k[1])
           []   k[2] = "inner" -> loc = k[1] /\ inner /\ ~Title(k[1])
           []   k[2] = "marker" -> FALSE
Exists(k) == loc = k[1] /\ (k[2] = "dir" \/ (k[2] = "marker" /\ marker # Absent)
                            \/ (k[2] = "inner" /\ inner))
Under(p) == {<<p, "marker">>, <<p, "inner">>}

TypeOK ==
  /\ loc \in Places \cup {Absent} /\ marker \in {Absent, "valid", "invalid"}
  /\ inner \in BOOLEAN /\ online \in BOOLEAN /\ watching \in BOOLEAN
  /\ events \subseteq Keys /\ dirty \subseteq Keys
  /\ pc \in {"idle", "probe", "commit"} /\ key \in Keys
  /\ seen \in [exists : BOOLEAN, unit : BOOLEAN, error : BOOLEAN, offline : BOOLEAN]
  /\ row \in [Keys -> BOOLEAN]

Init ==
  /\ loc = "A" /\ marker = Absent /\ inner = TRUE /\ online = TRUE /\ watching = TRUE
  /\ events = {} /\ dirty = Keys /\ pc = "idle" /\ key = <<"A", "dir">>
  /\ seen = [exists |-> FALSE, unit |-> FALSE, error |-> FALSE, offline |-> FALSE]
  /\ row = [k \in Keys |-> FALSE]
  /\ env = 0 /\ falseDeletes = 0

Emit(K) == events' = IF watching THEN events \cup K ELSE events

Request(K) == dirty' = dirty \cup K

EnvStep == env < MaxEnv /\ env' = env + 1

Unchanged == UNCHANGED <<dirty, pc, key, seen, row, falseDeletes>>

-----------------------------------------------------------------------------
\* The user's folder and drive.

SetMarker(m) ==
  /\ EnvStep /\ loc # Absent /\ marker # m
  /\ marker' = m
  /\ Emit({<<loc, "marker">>})
  /\ UNCHANGED <<loc, inner, online, watching>> /\ Unchanged

ToggleInner ==
  /\ EnvStep /\ loc # Absent
  /\ inner' = ~inner
  /\ Emit({<<loc, "inner">>})
  /\ UNCHANGED <<loc, marker, online, watching>> /\ Unchanged

\* A rename reports the source and, unless filtered, the destination folder.
Move ==
  /\ EnvStep /\ loc # Absent
  /\ loc' = IF loc = "A" THEN "B" ELSE "A"
  /\ Emit({<<loc, "dir">>} \cup IF KeepRenames THEN {<<loc', "dir">>} ELSE {})
  /\ UNCHANGED <<marker, inner, online, watching>> /\ Unchanged

Delete ==
  /\ EnvStep /\ loc # Absent
  /\ loc' = Absent
  /\ Emit({<<loc, "dir">>})
  /\ UNCHANGED <<marker, inner, online, watching>> /\ Unchanged

GoOffline ==
  /\ EnvStep /\ online
  /\ online' = FALSE /\ watching' = FALSE /\ events' = {}
  /\ UNCHANGED <<loc, marker, inner>> /\ Unchanged

\* Reconnecting re-arms the watch and rescans every key.
GoOnline ==
  /\ EnvStep /\ ~online
  /\ online' = TRUE /\ watching' = TRUE /\ dirty' = Keys
  /\ UNCHANGED <<loc, marker, inner, events, pc, key, seen, row, falseDeletes>>

-----------------------------------------------------------------------------
\* Watcher delivery and the reconcile worker.

Deliver(k) ==
  /\ k \in events
  /\ events' = events \ {k}
  /\ Request({k})
  /\ UNCHANGED <<loc, marker, inner, online, watching, pc, key, seen, row, env, falseDeletes>>

Take(k) ==
  /\ pc = "idle" /\ k \in dirty
  /\ dirty' = dirty \ {k}
  /\ pc' = "probe" /\ key' = k
  /\ seen' = [exists |-> online /\ Exists(k), unit |-> FALSE, error |-> ~online, offline |-> ~online]
  /\ UNCHANGED <<loc, marker, inner, online, watching, events, row, env, falseDeletes>>

Probe ==
  /\ pc = "probe"
  /\ pc' = "commit"
  /\ seen' = [seen EXCEPT !.unit = online /\ Unit(key),
                          !.error = @ \/ (~online /\ ProbeError),
                          !.offline = @ \/ ~online]
  /\ UNCHANGED <<loc, marker, inner, online, watching, events, dirty, key, row, env, falseDeletes>>

\* Whether following a marker to its folder can change anything.
Follow(p) == MarkerRule = "always" \/ row[<<p, "dir">>] # Title(p)

Commit ==
  LET k == key
      p == k[1]
      keep == ~seen.error /\ seen.exists /\ seen.unit
      drop == ~seen.error /\ ~keep
      expand == seen.exists /\ ~seen.unit /\ ~seen.error
      requests ==
        CASE k[2] = "dir" /\ ~seen.error ->
               (IF expand THEN {u \in Under(p) : ExpandMarkers \/ u[2] # "marker"} ELSE {})
               \cup {u \in Under(p) : row[u]}
        []   k[2] = "marker" /\ ~seen.error /\ (seen.exists \/ MarkerOnDelete) /\ Follow(p) ->
               {<<p, "dir">>}
        []   k[2] = "inner" /\ seen.exists /\ Title(p) -> {<<p, "dir">>}
        []   OTHER -> {}
  IN
  /\ pc = "commit"
  /\ pc' = "idle"
  /\ row' = [row EXCEPT ![k] = IF keep THEN TRUE ELSE IF drop THEN FALSE ELSE @]
  /\ falseDeletes' = falseDeletes + (IF row[k] /\ drop /\ seen.offline THEN 1 ELSE 0)
  /\ Request(requests)
  /\ UNCHANGED <<loc, marker, inner, online, watching, events, key, seen, env>>

Next ==
  \/ \E m \in {Absent, "valid", "invalid"} : SetMarker(m)
  \/ ToggleInner \/ Move \/ Delete \/ GoOffline \/ GoOnline
  \/ \E k \in Keys : Deliver(k) \/ Take(k)
  \/ Probe \/ Commit

Spec == Init /\ [][Next]_vars
        /\ \A k \in Keys : WF_vars(Deliver(k)) /\ WF_vars(Take(k))
        /\ WF_vars(Probe) /\ WF_vars(Commit)

-----------------------------------------------------------------------------

\* A row is never dropped because of a read from an unreachable drive.
NoFalseDelete == falseDeletes = 0

\* Reconciling stops once the folder stops changing.
Settles == <>[](dirty = {} /\ events = {} /\ pc = "idle")

\* Once settled on a reachable drive, the catalog holds exactly the units on disk.
Converges == <>[](online => \A k \in Keys : row[k] = Unit(k))

=============================================================================
