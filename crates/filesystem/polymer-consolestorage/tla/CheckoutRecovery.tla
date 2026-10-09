------------------------- MODULE CheckoutRecovery -------------------------
EXTENDS Integers, FiniteSets
CONSTANTS Saves, Files, Initial, Versions, Recovery, Shadow, Order, Dedupe, Registry,
          MaxOtherOpens, MaxWrites, MaxCrashes, MaxIndexLoss, MaxExternal, MaxRecoveryCrashes, MaxRestarts
Absent == "absent"
Torn == "torn"
None == [blob |-> Absent, stat |-> Absent]
Intact == "intact"
Contents == Versions \cup {Absent, Torn}
Paths == UNION {Files[s] : s \in Saves}
SaveOf(p) == CHOOSE s \in Saves : p \in Files[s]

VARIABLES phase, committed, checkout, index, overlay, lastGood, preserved, episode, stash,
          overwritten, observed, due, rolledBack, disturbed,
          writes, crashes, losses, external, recoveryCrashes, restarts, otherOpens
vars == <<phase, committed, checkout, index, overlay, lastGood, preserved, episode, stash,
          overwritten, observed, due, rolledBack, disturbed,
          writes, crashes, losses, external, recoveryCrashes, restarts, otherOpens>>
counters == <<writes, crashes, losses, external, recoveryCrashes, restarts, otherOpens>>
ghosts == <<overwritten, observed, due, rolledBack, disturbed>>
durable == <<preserved, episode>>

Committed == {p \in Paths : committed[p] # Absent}
IndexFor(files) == [p \in Paths |-> IF committed[p] = Absent THEN None
                                     ELSE [blob |-> committed[p], stat |-> files[p]]]
SnapshotOf(files, s) == [save |-> s, files |-> [p \in Files[s] |-> files[p]]]
DifferingSaves == {s \in Saves : \E p \in Files[s] : checkout[p] # committed[p]}
Covers(entries, s) ==
    \E entry \in entries :
        /\ entry.save = s
        /\ \A p \in Files[s] : checkout[p] \in {entry.files[p], committed[p]}
DedupeSource == CASE Dedupe = "episode" -> episode
                   [] Dedupe = "history" -> preserved
                   [] OTHER -> {}
Explained(s) == Covers(DedupeSource, s)
Due == {SnapshotOf(checkout, s) : s \in {t \in DifferingSaves : ~Covers(episode, t)}} \cap observed
Observe(files) == observed \cup {SnapshotOf(files, s) : s \in Saves}

Init ==
    /\ phase = "running"
    /\ committed = Initial /\ checkout = Initial
    /\ index = [p \in Paths |-> IF Initial[p] = Absent THEN None
                                ELSE [blob |-> Initial[p], stat |-> Initial[p]]]
    /\ overlay = {} /\ lastGood = Initial
    /\ preserved = {} /\ episode = {} /\ stash = {}
    /\ overwritten = {} /\ observed = {} /\ due = {}
    /\ rolledBack = TRUE /\ disturbed = FALSE /\ otherOpens = 0
    /\ writes = 0 /\ crashes = 0 /\ losses = 0 /\ external = 0
    /\ recoveryCrashes = 0 /\ restarts = 0

Write(p, v) ==
    /\ phase = "running" /\ writes < MaxWrites /\ checkout[p] # v
    /\ checkout' = [checkout EXCEPT ![p] = v]
    /\ overlay' = overlay \cup {p}
    /\ writes' = writes + 1
    /\ UNCHANGED <<phase, committed, index, lastGood, durable, stash, ghosts,
                   crashes, losses, external, recoveryCrashes, restarts, otherOpens>>

CommitPending ==
    /\ committed' = [p \in Paths |-> IF p \in overlay THEN checkout[p] ELSE committed[p]]
    /\ index' = [p \in Paths |-> IF committed'[p] = Absent THEN None
                                 ELSE [blob |-> committed'[p], stat |-> checkout[p]]]
    /\ overlay' = {}

Checkpoint ==
    /\ phase = "running" /\ overlay # {}
    /\ CommitPending
    /\ UNCHANGED <<phase, checkout, lastGood, durable, stash, ghosts, counters>>

Stop ==
    /\ phase = "running" /\ restarts < MaxRestarts
    /\ CommitPending
    /\ lastGood' = committed' /\ phase' = "stopped"
    /\ UNCHANGED <<checkout, durable, stash, ghosts, counters>>

SessionCrash(torn) ==
    /\ phase = "running" /\ crashes < MaxCrashes /\ restarts < MaxRestarts
    /\ crashes' = crashes + 1 /\ overlay' = {}
    /\ checkout' = IF torn = Intact \/ Shadow THEN checkout ELSE [checkout EXCEPT ![torn] = Torn]
    /\ observed' = Observe(checkout')
    /\ lastGood' = committed /\ phase' = "stopped"
    /\ UNCHANGED <<committed, index, durable, stash, overwritten, due, rolledBack, disturbed,
                   writes, losses, external, recoveryCrashes, restarts, otherOpens>>

IndexLoss ==
    /\ phase = "stopped" /\ losses < MaxIndexLoss
    /\ losses' = losses + 1 /\ index' = [p \in Paths |-> None]
    /\ UNCHANGED <<phase, committed, checkout, overlay, lastGood, durable, stash, ghosts,
                   writes, crashes, external, recoveryCrashes, restarts, otherOpens>>

ExternalChange(p, v) ==
    /\ phase = "stopped" /\ external < MaxExternal /\ checkout[p] # v
    /\ external' = external + 1 /\ checkout' = [checkout EXCEPT ![p] = v]
    /\ observed' = Observe(checkout')
    /\ UNCHANGED <<phase, committed, index, overlay, lastGood, durable, stash,
                   overwritten, due, rolledBack, disturbed,
                   writes, crashes, losses, recoveryCrashes, restarts, otherOpens>>

Reusable ==
    /\ \A p \in Paths : (index[p] = None) = (committed[p] = Absent)
    /\ \A p \in Committed : index[p].blob = committed[p] /\ checkout[p] # Absent

CheckoutTarget ==
    [p \in Paths |-> IF committed[p] = Absent THEN Absent
                     ELSE IF index[p] # None /\ index[p].blob = committed[p] /\ index[p].stat = checkout[p]
                          THEN checkout[p] ELSE committed[p]]

Restored ==
    CASE Recovery \in {"guard", "rescan"} /\ Committed # {} -> checkout
      [] Reusable -> checkout
      [] OTHER -> CheckoutTarget

EnsureCheckout ==
    /\ phase = "stopped" /\ Recovery # "preserve"
    /\ restarts' = restarts + 1 /\ phase' = "starting"
    /\ checkout' = Restored
    /\ index' = IF Restored = checkout /\ Reusable THEN index ELSE IndexFor(Restored)
    /\ overwritten' = overwritten \cup
         {[path |-> p, content |-> checkout[p]] : p \in {q \in Paths : checkout[q] # Restored[q]}}
    /\ due' = due \cup Due
    /\ overlay' = IF Recovery = "rescan" THEN {p \in Paths : Restored[p] # committed[p]} ELSE {}
    /\ UNCHANGED <<committed, lastGood, durable, stash, observed, rolledBack, disturbed,
                   writes, crashes, losses, external, recoveryCrashes, otherOpens>>

BeginRecovery ==
    /\ phase = "stopped" /\ Recovery = "preserve"
    /\ restarts' = restarts + 1
    /\ stash' = {SnapshotOf(checkout, s) : s \in {t \in DifferingSaves : ~Explained(t)}}
    /\ phase' = IF Order = "recordFirst" THEN "recording" ELSE "rollback"
    /\ UNCHANGED <<committed, checkout, index, overlay, lastGood, durable, overwritten, observed,
                   due, rolledBack, disturbed, writes, crashes, losses, external, recoveryCrashes, otherOpens>>

Record ==
    /\ phase = "recording"
    /\ preserved' = preserved \cup stash /\ episode' = episode \cup stash
    /\ phase' = IF Order = "recordFirst" THEN "rollback" ELSE "indexing"
    /\ UNCHANGED <<committed, checkout, index, overlay, lastGood, stash, ghosts, counters>>

RollbackPath(p) ==
    /\ phase = "rollback" /\ checkout[p] # committed[p]
    /\ checkout' = [checkout EXCEPT ![p] = committed[p]]
    /\ overwritten' = overwritten \cup {[path |-> p, content |-> checkout[p]]}
    /\ due' = IF SnapshotOf(checkout, SaveOf(p)) \in observed /\ ~Covers(episode, SaveOf(p))
              THEN due \cup {SnapshotOf(checkout, SaveOf(p))} ELSE due
    /\ UNCHANGED <<phase, committed, index, overlay, lastGood, durable, stash,
                   observed, rolledBack, disturbed, counters>>

RolledBack ==
    /\ phase = "rollback" /\ DifferingSaves = {}
    /\ phase' = IF Order = "recordFirst" THEN "indexing" ELSE "recording"
    /\ UNCHANGED <<committed, checkout, index, overlay, lastGood, durable, stash, ghosts, counters>>

WriteIndex ==
    /\ phase = "indexing"
    /\ index' = IndexFor(checkout) /\ stash' = {} /\ episode' = {}
    /\ phase' = "starting"
    /\ UNCHANGED <<committed, checkout, overlay, lastGood, preserved,
                   overwritten, observed, due, rolledBack, disturbed, counters>>

RecoveryCrash ==
    /\ phase \in {"recording", "rollback", "indexing"}
    /\ recoveryCrashes < MaxRecoveryCrashes /\ restarts < MaxRestarts
    /\ recoveryCrashes' = recoveryCrashes + 1
    /\ stash' = {} /\ phase' = "stopped"
    /\ UNCHANGED <<committed, checkout, index, overlay, lastGood, durable, ghosts,
                   writes, crashes, losses, external, restarts, otherOpens>>

OtherOpen ==
    /\ phase = "running" /\ ~Registry /\ otherOpens < MaxOtherOpens
    /\ otherOpens' = otherOpens + 1
    /\ LET opened == IF Recovery = "preserve" THEN committed ELSE Restored IN
        /\ checkout' = opened
        /\ disturbed' = (disturbed \/ \E p \in overlay : opened[p] # checkout[p])
        /\ overwritten' = overwritten \cup
             {[path |-> p, content |-> checkout[p]] : p \in {q \in Paths : opened[q] # checkout[q]}}
        /\ preserved' = IF Recovery = "preserve"
                         THEN preserved \cup {SnapshotOf(checkout, t) : t \in DifferingSaves}
                         ELSE preserved
        /\ index' = IndexFor(opened)
    /\ UNCHANGED <<phase, committed, overlay, lastGood, episode, stash, observed, due, rolledBack,
                   writes, crashes, losses, external, recoveryCrashes, restarts>>

Baseline ==
    /\ phase = "starting"
    /\ CommitPending
    /\ rolledBack' = (checkout = lastGood)
    /\ phase' = "running"
    /\ UNCHANGED <<checkout, lastGood, durable, stash, overwritten, observed, due, disturbed,
                   counters>>

Next ==
    \/ \E p \in Paths, v \in Versions \cup {Absent} : Write(p, v) \/ ExternalChange(p, v)
    \/ \E torn \in Paths \cup {Intact} : SessionCrash(torn)
    \/ \E p \in Paths : RollbackPath(p)
    \/ Checkpoint \/ Stop \/ IndexLoss \/ EnsureCheckout \/ Baseline
    \/ OtherOpen \/ BeginRecovery \/ Record \/ RolledBack \/ WriteIndex \/ RecoveryCrash
    \/ UNCHANGED vars

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ phase \in {"running", "stopped", "recording", "rollback", "indexing", "starting"}
    /\ committed \in [Paths -> Contents]
    /\ checkout \in [Paths -> Contents]
    /\ overlay \subseteq Paths
    /\ \A entry \in preserved \cup episode \cup stash : entry.save \in Saves
    /\ overwritten \subseteq [path : Paths, content : Contents]
    /\ rolledBack \in BOOLEAN
RestartsFromLastGood == rolledBack
NoTornCurrent == phase = "running" => \A p \in Paths : committed[p] # Torn /\ checkout[p] # Torn
NothingDestroyed == phase = "running" =>
    \A gone \in overwritten : \E entry \in preserved :
        entry.save = SaveOf(gone.path) /\ entry.files[gone.path] = gone.content
SavesPreserved == phase = "running" => due \subseteq preserved
EntriesExisted == preserved \subseteq observed
PreservedComplete == \A entry \in preserved : \A p \in DOMAIN entry.files : entry.files[p] # Torn
LiveSessionUndisturbed == ~disturbed
ProfileTracksCheckout == phase = "running" /\ overlay = {} => committed = checkout
=============================================================================
