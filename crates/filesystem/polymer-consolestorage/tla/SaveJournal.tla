---------------------------- MODULE SaveJournal ----------------------------
(***************************************************************************)
(* T3: save-history materialization journal.                              *)
(*                                                                         *)
(* Rust mapping (history/transaction.rs, history/mod.rs, archive.rs):      *)
(*   journal        refs/consoles/transactions/v2/<volume>                 *)
(*                  (write_journal / read_journal / clear_journal)         *)
(*   fs             files in the owning Storage (install_file / remove)    *)
(*   head, attach   save + notes refs, edited atomically by                *)
(*                  install_save_refs                                      *)
(*   profileTip     profile.set_save_tip + commit_pending                  *)
(*   lock           HistoryInner::operation                                *)
(*   ctx[p]         one apply_files / apply_files_once loop in flight      *)
(*                                                                         *)
(* Callers (restore, restore_deleted, archive_missing_saves) call          *)
(* recover() and release the lock before taking it again for               *)
(* apply_transition_locked; that window is modelled by RecoverThenLock.    *)
(* Snapshot 0 is "no save" (before/after = None).                          *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Procs, Paths, Snapshots, Contents, InitSnap, Target,
          MaxFailures, MaxCrashes

ASSUME InitSnap \in Snapshots /\ 0 \in Snapshots
ASSUME Target \in [Procs -> Snapshots]
ASSUME Contents \in [Snapshots -> SUBSET Paths] /\ Contents[0] = {}

None == "none"
NoJournal == [old |-> -1, target |-> -1]

VARIABLES fs, head, attach, profileTip, journal, lock,
          pc, ctx, done, failures, crashes

vars == <<fs, head, attach, profileTip, journal, lock, pc, ctx, done, failures, crashes>>

Mat(s) == [p \in Paths |-> IF p \in Contents[s] THEN s ELSE 0]

\* One apply_files_once(src, dst): install every dst file, then remove src-only files.
Pass(cont, stage, src0, dst0, src, dst) ==
  [cont |-> cont, stage |-> stage, src0 |-> src0, dst0 |-> dst0,
   dst |-> dst, install |-> Contents[dst], remove |-> Contents[src] \ Contents[dst]]

\* ctx.stage: "fwd" = first apply_files_once, "back" = its rollback.
\* ctx.cont:  "main" = materialization, "comp" = compensation after refs failed.
NoCtx == Pass("none", "none", 0, 0, 0, 0)

Start(cont, src, dst) == Pass(cont, "fwd", src, dst, src, dst)

TypeOK ==
  /\ fs \in [Paths -> Snapshots]
  /\ head \in Snapshots /\ attach \in Snapshots /\ profileTip \in Snapshots
  /\ lock \in Procs \cup {None}
  /\ pc \in [Procs -> {"idle", "recovering", "want", "files", "refs", "profile", "clear"}]

Init ==
  /\ fs = Mat(InitSnap)
  /\ head = InitSnap /\ attach = InitSnap /\ profileTip = InitSnap
  /\ journal = NoJournal
  /\ lock = None
  /\ pc = [p \in Procs |-> "idle"]
  /\ ctx = [p \in Procs |-> NoCtx]
  /\ done = [p \in Procs |-> FALSE]
  /\ failures = 0 /\ crashes = 0

CanFail == failures < MaxFailures

Release(p) == /\ lock' = None /\ pc' = [pc EXCEPT ![p] = "idle"] /\ ctx' = [ctx EXCEPT ![p] = NoCtx]

\* recover() with no journal: lock, read nothing, unlock.  With a journal:
\* run recover_locked.  The caller then later takes the lock again.
RecoverThenLock(p) ==
  /\ pc[p] = "idle" /\ ~done[p] /\ lock = None
  /\ journal = NoJournal
  /\ pc' = [pc EXCEPT ![p] = "want"]
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, lock, ctx, done, failures, crashes>>

\* apply_transition_locked: write_journal (before = current refs) then recover_locked.
WriteJournal(p) ==
  /\ pc[p] = "want" /\ lock = None
  /\ lock' = p
  /\ journal' = [old |-> head, target |-> Target[p]]
  /\ done' = [done EXCEPT ![p] = TRUE]
  /\ pc' = [pc EXCEPT ![p] = "files"]
  /\ ctx' = [ctx EXCEPT ![p] = Start("main", head, Target[p])]
  /\ UNCHANGED <<fs, head, attach, profileTip, failures, crashes>>

\* recover() at startup or from any later caller when a journal exists.
Recover(p) ==
  /\ pc[p] = "idle" /\ lock = None /\ journal # NoJournal
  /\ lock' = p
  /\ pc' = [pc EXCEPT ![p] = "files"]
  /\ ctx' = [ctx EXCEPT ![p] = Start("main", journal.old, journal.target)]
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, done, failures, crashes>>

InstallFile(p) ==
  /\ pc[p] = "files"
  /\ \E f \in ctx[p].install :
       /\ fs' = [fs EXCEPT ![f] = ctx[p].dst]
       /\ ctx' = [ctx EXCEPT ![p].install = @ \ {f}]
  /\ UNCHANGED <<head, attach, profileTip, journal, lock, pc, done, failures, crashes>>

RemoveFile(p) ==
  /\ pc[p] = "files" /\ ctx[p].install = {}
  /\ \E f \in ctx[p].remove :
       /\ fs' = [fs EXCEPT ![f] = 0]
       /\ ctx' = [ctx EXCEPT ![p].remove = @ \ {f}]
  /\ UNCHANGED <<head, attach, profileTip, journal, lock, pc, done, failures, crashes>>

\* Outcome of apply_files once the current apply_files_once finished or failed.
\*   ok          : fwd pass finished
\*   rolledBack  : fwd failed, back pass finished
\*   rollbackFail: fwd failed, back failed
Outcome(p, kind) ==
  LET c == ctx[p] IN
  IF c.cont = "main" THEN
    CASE kind = "ok" ->
           /\ pc' = [pc EXCEPT ![p] = "refs"] /\ ctx' = [ctx EXCEPT ![p] = c]
           /\ UNCHANGED <<journal, lock>>
      [] kind = "rolledBack" ->
           /\ journal' = NoJournal /\ Release(p)
      [] kind = "rollbackFail" ->
           /\ UNCHANGED journal /\ Release(p)
  ELSE
    CASE kind = "ok" ->
           /\ journal' = NoJournal /\ Release(p)
      [] kind \in {"rolledBack", "rollbackFail"} ->
           /\ UNCHANGED journal /\ Release(p)

FilesDone(p) ==
  /\ pc[p] = "files" /\ ctx[p].install = {} /\ ctx[p].remove = {}
  /\ Outcome(p, IF ctx[p].stage = "fwd" THEN "ok" ELSE "rolledBack")
  /\ UNCHANGED <<fs, head, attach, profileTip, done, failures, crashes>>

FileFails(p) ==
  /\ pc[p] = "files" /\ CanFail
  /\ ctx[p].install \cup ctx[p].remove # {}
  /\ failures' = failures + 1
  /\ IF ctx[p].stage = "fwd"
       THEN /\ ctx' = [ctx EXCEPT ![p] = Pass(ctx[p].cont, "back", ctx[p].src0, ctx[p].dst0,
                                              ctx[p].dst0, ctx[p].src0)]
            /\ UNCHANGED <<journal, lock, pc>>
       ELSE Outcome(p, "rollbackFail")
  /\ UNCHANGED <<fs, head, attach, profileTip, done, crashes>>

\* install_save_refs: one atomic edit of head + every notes ref.
InstallRefs(p) ==
  /\ pc[p] = "refs"
  /\ head' = journal.target /\ attach' = journal.target
  /\ pc' = [pc EXCEPT ![p] = "profile"]
  /\ UNCHANGED <<fs, profileTip, journal, lock, ctx, done, failures, crashes>>

RefsFail(p) ==
  /\ pc[p] = "refs" /\ CanFail
  /\ failures' = failures + 1
  /\ pc' = [pc EXCEPT ![p] = "files"]
  /\ ctx' = [ctx EXCEPT ![p] = Start("comp", journal.target, journal.old)]
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, lock, done, crashes>>

CommitProfile(p) ==
  /\ pc[p] = "profile"
  /\ profileTip' = journal.target
  /\ pc' = [pc EXCEPT ![p] = "clear"]
  /\ UNCHANGED <<fs, head, attach, journal, lock, ctx, done, failures, crashes>>

ProfileFails(p) ==
  /\ pc[p] = "profile" /\ CanFail
  /\ failures' = failures + 1
  /\ Release(p)
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, done, crashes>>

ClearJournal(p) ==
  /\ pc[p] = "clear"
  /\ journal' = NoJournal
  /\ Release(p)
  /\ UNCHANGED <<fs, head, attach, profileTip, done, failures, crashes>>

ClearFails(p) ==
  /\ pc[p] = "clear" /\ CanFail
  /\ failures' = failures + 1
  /\ Release(p)
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, done, crashes>>

\* Process death: volatile state is lost, durable state survives.
Crash ==
  /\ crashes < MaxCrashes
  /\ \E p \in Procs : pc[p] # "idle"
  /\ crashes' = crashes + 1
  /\ lock' = None
  /\ pc' = [p \in Procs |-> "idle"]
  /\ ctx' = [p \in Procs |-> NoCtx]
  /\ UNCHANGED <<fs, head, attach, profileTip, journal, done, failures>>

Progress(p) ==
  \/ WriteJournal(p) \/ Recover(p) \/ InstallFile(p) \/ RemoveFile(p) \/ FilesDone(p)
  \/ InstallRefs(p) \/ CommitProfile(p) \/ ClearJournal(p)

Faults(p) == FileFails(p) \/ RefsFail(p) \/ ProfileFails(p) \/ ClearFails(p)

Finished ==
  /\ \A p \in Procs : done[p] /\ pc[p] = "idle"
  /\ journal = NoJournal
  /\ UNCHANGED vars

Next ==
  \/ \E p \in Procs : RecoverThenLock(p) \/ Progress(p) \/ Faults(p)
  \/ Crash
  \/ Finished

Spec == Init /\ [][Next]_vars /\ \A p \in Procs : WF_vars(Progress(p))

Quiescent == journal = NoJournal /\ lock = None

\* After every completed or rolled-back transition, the depot is exactly the
\* snapshot its refs name.
MaterializedMatchesRefs == Quiescent => fs = Mat(head)

\* The profile's recorded tip agrees with the refs when nothing is pending.
ProfileMatchesRefs == Quiescent => profileTip = head

\* Notes refs always move with the save ref.
AttachmentsFollowTip == attach = head

\* While a journal is pending the refs are at old or target.
RefsAtEndpoint == journal # NoJournal => head \in {journal.old, journal.target}

\* A pending journal is eventually resolved.
JournalResolves == journal # NoJournal ~> journal = NoJournal

=============================================================================
