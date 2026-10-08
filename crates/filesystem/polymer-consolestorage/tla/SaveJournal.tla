---------------------------- MODULE SaveJournal ----------------------------
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Procs, Depots, Depot, Paths, Snapshots, Contents, InitSnap, Target,
          MaxFailures, MaxCrashes

ASSUME InitSnap \in Snapshots /\ 0 \in Snapshots
ASSUME Target \in [Procs -> Snapshots] /\ Depot \in [Procs -> Depots]
ASSUME Contents \in [Snapshots -> SUBSET Paths] /\ Contents[0] = {}

None == "none"
NoJournal == [depot |-> None, old |-> -1, target |-> -1]
Notes == {"note", "screenshot", "icon", "title"}
RefNames == Notes \cup {"head"}

VARIABLES fs, refs, profileTip, journal, lock, pc, todo, done, failures, crashes
vars == <<fs, refs, profileTip, journal, lock, pc, todo, done, failures, crashes>>

Mat(s) == [p \in Paths |-> IF p \in Contents[s] THEN s ELSE 0]
Work(old, target) == [install |-> Contents[target],
                       remove |-> Contents[old] \ Contents[target], refs |-> RefNames]
NoWork == Work(0, 0)

TypeOK ==
  /\ fs \in [Depots -> [Paths -> Snapshots]]
  /\ refs \in [Depots -> [RefNames -> Snapshots]]
  /\ profileTip \in [Depots -> Snapshots]
  /\ journal \in {NoJournal} \cup [depot : Depots, old : Snapshots, target : Snapshots]
  /\ lock \in Procs \cup {None}
  /\ pc \in [Procs -> {"idle", "want", "prepared", "files", "refs", "profile", "clear"}]

Init ==
  /\ fs = [d \in Depots |-> Mat(InitSnap)]
  /\ refs = [d \in Depots |-> [r \in RefNames |-> InitSnap]]
  /\ profileTip = [d \in Depots |-> InitSnap]
  /\ journal = NoJournal /\ lock = None
  /\ pc = [p \in Procs |-> "idle"]
  /\ todo = NoWork /\ done = [p \in Procs |-> FALSE]
  /\ failures = 0 /\ crashes = 0

Release(p) == /\ lock' = None /\ pc' = [pc EXCEPT ![p] = "idle"] /\ todo' = NoWork

Request(p) ==
  /\ pc[p] = "idle" /\ ~done[p]
  /\ pc' = [pc EXCEPT ![p] = "want"]
  /\ UNCHANGED <<fs, refs, profileTip, journal, lock, todo, done, failures, crashes>>

Prepare(p) ==
  /\ pc[p] = "want" /\ lock = None /\ journal = NoJournal
  /\ lock' = p /\ pc' = [pc EXCEPT ![p] = "prepared"]
  /\ UNCHANGED <<fs, refs, profileTip, journal, todo, done, failures, crashes>>

WriteJournal(p) ==
  /\ pc[p] = "prepared" /\ lock = p /\ journal = NoJournal
  /\ journal' = [depot |-> Depot[p], old |-> refs[Depot[p]]["head"], target |-> Target[p]]
  /\ todo' = Work(refs[Depot[p]]["head"], Target[p])
  /\ pc' = [pc EXCEPT ![p] = "files"] /\ done' = [done EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<fs, refs, profileTip, lock, failures, crashes>>

Recover(p) ==
  /\ pc[p] \in {"idle", "want"} /\ lock = None /\ journal # NoJournal
  /\ lock' = p /\ pc' = [pc EXCEPT ![p] = "files"]
  /\ todo' = Work(journal.old, journal.target)
  /\ UNCHANGED <<fs, refs, profileTip, journal, done, failures, crashes>>

InstallFile(p) ==
  /\ pc[p] = "files"
  /\ \E f \in todo.install :
       /\ fs' = [fs EXCEPT ![journal.depot][f] = journal.target]
       /\ todo' = [todo EXCEPT !.install = @ \ {f}]
  /\ UNCHANGED <<refs, profileTip, journal, lock, pc, done, failures, crashes>>

RemoveFile(p) ==
  /\ pc[p] = "files" /\ todo.install = {}
  /\ \E f \in todo.remove :
       /\ fs' = [fs EXCEPT ![journal.depot][f] = 0]
       /\ todo' = [todo EXCEPT !.remove = @ \ {f}]
  /\ UNCHANGED <<refs, profileTip, journal, lock, pc, done, failures, crashes>>

FilesDone(p) ==
  /\ pc[p] = "files" /\ todo.install = {} /\ todo.remove = {}
  /\ pc' = [pc EXCEPT ![p] = "refs"]
  /\ UNCHANGED <<fs, refs, profileTip, journal, lock, todo, done, failures, crashes>>

InstallRef(p) ==
  /\ pc[p] = "refs"
  /\ \E r \in todo.refs :
       /\ refs' = [refs EXCEPT ![journal.depot][r] = journal.target]
       /\ todo' = [todo EXCEPT !.refs = @ \ {r}]
  /\ UNCHANGED <<fs, profileTip, journal, lock, pc, done, failures, crashes>>

RefsDone(p) ==
  /\ pc[p] = "refs" /\ todo.refs = {}
  /\ pc' = [pc EXCEPT ![p] = "profile"]
  /\ UNCHANGED <<fs, refs, profileTip, journal, lock, todo, done, failures, crashes>>

CommitProfile(p) ==
  /\ pc[p] = "profile"
  /\ profileTip' = [profileTip EXCEPT ![journal.depot] = journal.target]
  /\ pc' = [pc EXCEPT ![p] = "clear"]
  /\ UNCHANGED <<fs, refs, journal, lock, todo, done, failures, crashes>>

ClearJournal(p) ==
  /\ pc[p] = "clear"
  /\ journal' = NoJournal /\ Release(p)
  /\ UNCHANGED <<fs, refs, profileTip, done, failures, crashes>>

Fail(p) ==
  /\ pc[p] \in {"files", "refs", "profile", "clear"} /\ failures < MaxFailures
  /\ failures' = failures + 1 /\ Release(p)
  /\ UNCHANGED <<fs, refs, profileTip, journal, done, crashes>>

Crash ==
  /\ crashes < MaxCrashes /\ lock # None
  /\ crashes' = crashes + 1 /\ lock' = None
  /\ pc' = [p \in Procs |-> "idle"] /\ todo' = NoWork
  /\ UNCHANGED <<fs, refs, profileTip, journal, done, failures>>

Progress(p) ==
  \/ Request(p) \/ Prepare(p) \/ WriteJournal(p) \/ Recover(p)
  \/ InstallFile(p) \/ RemoveFile(p) \/ FilesDone(p)
  \/ InstallRef(p) \/ RefsDone(p) \/ CommitProfile(p) \/ ClearJournal(p)

Finished ==
  /\ \A p \in Procs : done[p] /\ pc[p] = "idle"
  /\ journal = NoJournal /\ UNCHANGED vars

Next == (\E p \in Procs : Progress(p) \/ Fail(p)) \/ Crash \/ Finished
Spec == Init /\ [][Next]_vars /\ \A p \in Procs : WF_vars(Progress(p))

Settled(d) == journal = NoJournal \/ journal.depot # d
MaterializedMatchesRefs == \A d \in Depots : Settled(d) => fs[d] = Mat(refs[d]["head"])
ProfileMatchesRefs == \A d \in Depots : Settled(d) => profileTip[d] = refs[d]["head"]
AttachmentsFollowTip == \A d \in Depots : Settled(d) => \A n \in Notes : refs[d][n] = refs[d]["head"]
RefsAtEndpoint == journal # NoJournal =>
  \A r \in RefNames : refs[journal.depot][r] \in {journal.old, journal.target}
ClearedAtTarget == (journal # NoJournal /\ journal' = NoJournal) =>
  /\ fs'[journal.depot] = Mat(journal.target)
  /\ profileTip'[journal.depot] = journal.target
  /\ \A r \in RefNames : refs'[journal.depot][r] = journal.target
JournalNotReplaced == [][journal # NoJournal => journal' \in {journal, NoJournal}]_vars
JournalCompletes == [][ClearedAtTarget]_vars
JournalResolves == journal # NoJournal ~> journal = NoJournal
=============================================================================
