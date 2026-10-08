---------------------------- MODULE NandUninstall ----------------------------
(***************************************************************************)
(* NAND install / uninstall racing guest writes in git-backed storage.    *)
(*                                                                         *)
(* Rust mapping (src/storage/git/ unless noted):                           *)
(*   checkout   files under GitVolumeInner.checkout (DirectoryStorage);    *)
(*              value = content version, Absent = no file                  *)
(*   idx        the NAND's slice of the platform index file                *)
(*              (checkout.rs write_index / indexed_checkout): per path the *)
(*              blob id and the stat taken when it was written; a stat     *)
(*              "matches" iff the file still holds the content it held     *)
(*              when write_index ran (stat_matches)                         *)
(*   trees      git commit objects (files/* + meta/artifacts/{A,B})        *)
(*   ref        refs/consoles/nands/v2/<id> (tip_in)                       *)
(*   reflog     reflog of that ref (LogChange force_create_reflog)         *)
(*   opLock     PlatformRepository operation Mutex (platform.0.operation)  *)
(*   lease      polymer-vfs ExclusiveResourceKey::Resource(nand id),       *)
(*              taken by NandVolumeFactory::materialize (mount) and by the *)
(*              polymer-runtime consolestorage Uninstall handler           *)
(*              (operations.rs) and install_job (jobs.rs)                  *)
(*   gWorking   GitVolumeInner.working.entries of the handle the emulator  *)
(*              mounted (every PlatformRepository::volume() call builds a  *)
(*              fresh GitVolumeInner, so overlays are per handle)          *)
(*   opened     files the guest holds open for write (GitVolumeFile)       *)
(*   gpc, gArg, gVal  guest thread inside traits.rs Storage::create        *)
(*              (checkout.create, then working.write().insert, no lock)    *)
(*   gAck       newest content per path whose create / write_at returned  *)
(*              Ok to the guest; reset only by an explicit recover()       *)
(*   ipc, iWorking, iMeta, iNew                                            *)
(*              install_job -> StorageManager::install ->                   *)
(*              Installation::prepare (is_dirty) -> install_file (traits.rs)*)
(*              -> GitArtifactRegistry::record (registry.rs: metadata      *)
(*              insert before the lock) -> commit_pending_reporting        *)
(*              (commit.rs: lock, tip_in, tree, CAS ExistingMustMatch,     *)
(*              write_index, clear working); on its own handle             *)
(*   upc, uOld, uNew, uTodo                                                *)
(*              Uninstall handler (lease, is_dirty) ->                     *)
(*              GitArtifactRegistry::uninstall (registry.rs: lock, working *)
(*              empty check, tip, affected) -> rewrite_without             *)
(*              (artifact/history.rs: write rebased commits, CAS           *)
(*              MustExistAndMatch(old_tip), then delete reflog) ->         *)
(*              refresh_checkout_locked (checkout.rs: checkout_target one  *)
(*              path per step, then write_index); on its own handle        *)
(*   CheckoutMatches  VolumeStorage::is_dirty / checkout_matches_commit    *)
(*              (index fast path: id and stat match, else hash the file)   *)
(*   Crash      process death: overlays, open handles, locks, leases and   *)
(*              in-flight jobs are lost; checkout, index, objects, ref and *)
(*              reflog survive                                             *)
(*   Reopen     next GitVolumeStorage: ensure_checkout on a NAND with a    *)
(*              tip only sets cache_ready (no repair)                      *)
(*   Recover    Recover handler -> VolumeStorage::recover                  *)
(*              (refresh_checkout_locked on the tip, clear overlay)        *)
(* rewrite_without is atomic and may fail (conflict); affected_depots is   *)
(* assumed empty. Paths: "app" belongs to installed artifact A (the one    *)
(* uninstalled), "inst" to artifact B (being installed), GuestPaths are    *)
(* written by the guest. rename / remove / create_dir_all follow the same  *)
(* checkout-then-overlay pattern as create and are not modelled.           *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS GuestPaths, MaxWrites, MaxCrashes, MaxFailures, MaxMounts, MaxRecovers,
          UseLease, CheckDirty, SharedHandle

ASSUME "sys" \in GuestPaths /\ "g" \in GuestPaths
ASSUME UseLease \in BOOLEAN /\ CheckDirty \in BOOLEAN /\ SharedHandle \in BOOLEAN

Absent == -1
InstallVal == MaxWrites + 1
NoEntry == [id |-> Absent, stat |-> Absent]
Paths == GuestPaths \cup {"app", "inst"}
Tree(f, a, b) == [files |-> f, recA |-> a, recB |-> b]
T0 == Tree([p \in Paths |-> IF p \in {"sys", "app"} THEN 0 ELSE Absent], TRUE, FALSE)

VARIABLES checkout, idx, trees, ref, reflog, opLock, lease,
          mounted, opened, gWorking, gpc, gArg, gVal, gAck, nextVal,
          ipc, iWorking, iMeta, iNew, iResult,
          upc, uOld, uNew, uTodo, uResult,
          up, crashes, failures, mounts, recovers

disk   == <<checkout, idx, trees, ref, reflog>>
guest  == <<mounted, opened, gWorking, gpc, gArg, gVal, gAck, nextVal>>
inst   == <<ipc, iWorking, iMeta, iNew, iResult>>
unin   == <<upc, uOld, uNew, uTodo, uResult>>
ctl    == <<opLock, lease, up, crashes, failures, mounts, recovers>>
vars   == <<disk, guest, inst, unin, ctl>>

Results == {"none", "ok", "refused", "error", "crashed"}

TypeOK ==
  /\ checkout \in [Paths -> {Absent} \cup 0..InstallVal]
  /\ idx \in [Paths -> [id : {Absent} \cup 0..InstallVal, stat : {Absent} \cup 0..InstallVal]]
  /\ ref \in 1..Len(trees) /\ reflog \subseteq 1..Len(trees)
  /\ opLock \in {"none", "install", "uninstall"}
  /\ lease \in {"none", "guest", "install", "uninstall"}
  /\ gpc \in {"idle", "create"} /\ gWorking \subseteq GuestPaths /\ opened \subseteq gWorking
  /\ gAck \in [GuestPaths -> {Absent} \cup 1..MaxWrites]
  /\ ipc \in {"idle", "check", "file", "overlay", "record", "lock", "index", "done"}
  /\ upc \in {"idle", "check", "lock", "rewrite", "cas", "reflog", "checkout", "index", "done"}
  /\ iResult \in Results /\ uResult \in Results

Stamp(t) == [p \in Paths |-> IF t.files[p] = Absent THEN NoEntry
                             ELSE [id |-> t.files[p], stat |-> checkout[p]]]

Init ==
  /\ checkout = T0.files
  /\ idx = [p \in Paths |-> IF T0.files[p] = Absent THEN NoEntry
                            ELSE [id |-> T0.files[p], stat |-> T0.files[p]]]
  /\ trees = <<T0>> /\ ref = 1 /\ reflog = {1}
  /\ opLock = "none" /\ lease = "none"
  /\ mounted = FALSE /\ opened = {} /\ gWorking = {} /\ gpc = "idle"
  /\ gArg = (CHOOSE p \in GuestPaths : TRUE) /\ gVal = Absent
  /\ gAck = [p \in GuestPaths |-> Absent] /\ nextVal = 1
  /\ ipc = "idle" /\ iWorking = {} /\ iMeta = FALSE /\ iNew = 1 /\ iResult = "none"
  /\ upc = "idle" /\ uOld = 1 /\ uNew = 1 /\ uTodo = {} /\ uResult = "none"
  /\ up = TRUE /\ crashes = 0 /\ failures = 0 /\ mounts = 0 /\ recovers = 0

\* checkout_matches_commit: index fast path (id + stat), else hash the file.
PathMatches(t, p) ==
  IF t.files[p] = Absent THEN checkout[p] = Absent
  ELSE /\ checkout[p] # Absent
       /\ \/ idx[p].id = t.files[p] /\ idx[p].stat = checkout[p]
          \/ checkout[p] = t.files[p]
CheckoutMatches(t) == \A p \in Paths : PathMatches(t, p)

\* is_dirty on a handle whose overlay is `w`.
Dirty(w) == w # {} \/ ~CheckoutMatches(trees[ref])

\* checkout_target for one path: skip when the index says it is current.
TargetPath(t, p) ==
  IF t.files[p] = Absent THEN Absent
  ELSE IF checkout[p] # Absent /\ idx[p].id = t.files[p] /\ idx[p].stat = checkout[p]
         THEN checkout[p]
         ELSE t.files[p]

Lease(who) == IF UseLease THEN lease' = who ELSE UNCHANGED lease
Release(who) == IF UseLease /\ lease = who THEN lease' = "none" ELSE UNCHANGED lease

----------------------------------------------------------------------------
\* Emulator: NandVolumeFactory::materialize / NandStorageLease::finalize.

Mount ==
  /\ up /\ ~mounted /\ mounts < MaxMounts /\ lease = "none"
  /\ mounted' = TRUE /\ lease' = "guest" /\ mounts' = mounts + 1
  /\ UNCHANGED <<disk, opened, gWorking, gpc, gArg, gVal, gAck, nextVal, inst, unin,
                 opLock, up, crashes, failures, recovers>>

\* Storage::create: checkout.create first.
GuestCreate(p) ==
  /\ up /\ mounted /\ gpc = "idle" /\ nextVal <= MaxWrites
  /\ checkout' = [checkout EXCEPT ![p] = nextVal]
  /\ gAck' = [gAck EXCEPT ![p] = Absent]
  /\ gpc' = "create" /\ gArg' = p /\ gVal' = nextVal /\ nextVal' = nextVal + 1
  /\ UNCHANGED <<idx, trees, ref, reflog, mounted, opened, gWorking, inst, unin, ctl>>

\* Storage::create: then working.write().entries.insert(File), returns Ok.
GuestCreateOverlay ==
  /\ gpc = "create"
  /\ gWorking' = gWorking \cup {gArg} /\ opened' = opened \cup {gArg}
  /\ gAck' = [gAck EXCEPT ![gArg] = gVal]
  /\ gpc' = "idle"
  /\ UNCHANGED <<disk, mounted, gArg, gVal, nextVal, inst, unin, ctl>>

\* GitVolumeFile::write_at on an open handle: checkout only, overlay untouched.
GuestWriteAt(p) ==
  /\ up /\ mounted /\ gpc = "idle" /\ p \in opened /\ nextVal <= MaxWrites
  /\ checkout[p] # Absent
  /\ checkout' = [checkout EXCEPT ![p] = nextVal]
  /\ gAck' = [gAck EXCEPT ![p] = nextVal] /\ nextVal' = nextVal + 1
  /\ UNCHANGED <<idx, trees, ref, reflog, mounted, opened, gWorking, gpc, gArg, gVal,
                 inst, unin, ctl>>

\* Emulator exit: finish() is a no-op, the mounted handle (and its overlay) is dropped.
Unmount ==
  /\ mounted /\ gpc = "idle"
  /\ mounted' = FALSE /\ opened' = {} /\ gWorking' = {}
  /\ lease' = IF lease = "guest" THEN "none" ELSE lease
  /\ UNCHANGED <<disk, gpc, gArg, gVal, gAck, nextVal, inst, unin,
                 opLock, up, crashes, failures, mounts, recovers>>

----------------------------------------------------------------------------
\* install_job: lease, then Installation::prepare.

IStart ==
  /\ up /\ ipc = "idle"
  /\ IF UseLease /\ lease # "none"
       THEN ipc' = "done" /\ iResult' = "refused" /\ UNCHANGED lease
       ELSE ipc' = "check" /\ Lease("install") /\ UNCHANGED iResult
  /\ UNCHANGED <<disk, guest, iWorking, iMeta, iNew, unin,
                 opLock, up, crashes, failures, mounts, recovers>>

IFail(r) ==
  /\ ipc' = "done" /\ iResult' = r /\ Release("install")

\* prepare: nand.is_dirty() (takes and releases the operation lock).
IDirtyCheck ==
  /\ ipc = "check" /\ opLock = "none"
  /\ IF CheckDirty /\ Dirty(iWorking)
       THEN IFail("refused")
       ELSE ipc' = "file" /\ UNCHANGED <<iResult, lease>>
  /\ UNCHANGED <<disk, guest, iWorking, iMeta, iNew, unin,
                 opLock, up, crashes, failures, mounts, recovers>>

\* install_file: checkout.install_file, then working insert (no lock).
IInstallFile ==
  /\ ipc = "file"
  /\ checkout' = [checkout EXCEPT !["inst"] = InstallVal]
  /\ ipc' = "overlay"
  /\ UNCHANGED <<idx, trees, ref, reflog, guest, iWorking, iMeta, iNew, iResult, unin, ctl>>

IInstallOverlay ==
  /\ ipc = "overlay"
  /\ iWorking' = iWorking \cup {"inst"} /\ ipc' = "record"
  /\ UNCHANGED <<disk, guest, iMeta, iNew, iResult, unin, ctl>>

\* record: working.metadata.artifacts.insert before any lock.
IRecord ==
  /\ ipc = "record"
  /\ iMeta' = TRUE /\ ipc' = "lock"
  /\ UNCHANGED <<disk, guest, iWorking, iNew, iResult, unin, ctl>>

\* commit_pending_reporting: lock, tip_in, build tree from overlay + checkout,
\* new_commit_as, CAS ExistingMustMatch(parent).
ICommit ==
  /\ ipc = "lock" /\ opLock = "none"
  /\ LET base == trees[ref]
         t == Tree([p \in Paths |-> IF p \in iWorking THEN checkout[p] ELSE base.files[p]],
                   base.recA, base.recB \/ iMeta)
     IN IF \E p \in iWorking : checkout[p] = Absent
          THEN /\ IFail("error")
               /\ UNCHANGED <<trees, ref, reflog, iNew, opLock>>
          ELSE /\ trees' = Append(trees, t) /\ ref' = Len(trees) + 1
               /\ reflog' = reflog \cup {Len(trees) + 1} /\ iNew' = Len(trees) + 1
               /\ opLock' = "install" /\ ipc' = "index"
               /\ UNCHANGED <<iResult, lease>>
  /\ UNCHANGED <<checkout, idx, guest, iWorking, iMeta, unin,
                 up, crashes, failures, mounts, recovers>>

\* commit_pending_reporting: write_index(committed tree), clear working, unlock.
ICommitIndex ==
  /\ ipc = "index"
  /\ IF \E p \in Paths : trees[iNew].files[p] # Absent /\ checkout[p] = Absent
       THEN /\ IFail("error") /\ UNCHANGED <<idx, iWorking, iMeta>>
       ELSE /\ idx' = Stamp(trees[iNew]) /\ iWorking' = {} /\ iMeta' = FALSE
            /\ ipc' = "done" /\ iResult' = "ok" /\ Release("install")
  /\ opLock' = "none"
  /\ UNCHANGED <<checkout, trees, ref, reflog, guest, iNew, unin,
                 up, crashes, failures, mounts, recovers>>

----------------------------------------------------------------------------
\* polymer-runtime Uninstall handler, then GitArtifactRegistry::uninstall.

UStart ==
  /\ up /\ upc = "idle"
  /\ IF UseLease /\ lease # "none"
       THEN upc' = "done" /\ uResult' = "refused" /\ UNCHANGED lease
       ELSE upc' = "check" /\ Lease("uninstall") /\ UNCHANGED uResult
  /\ UNCHANGED <<disk, guest, inst, uOld, uNew, uTodo,
                 opLock, up, crashes, failures, mounts, recovers>>

UFail(r) == upc' = "done" /\ uResult' = r /\ Release("uninstall")

\* Overlay seen by registry.rs working.read().is_empty() on the uninstall handle.
UOverlay == IF SharedHandle THEN gWorking \cup iWorking ELSE {}
UMeta == SharedHandle /\ iMeta

\* Handler: nand.is_dirty().
UDirtyCheck ==
  /\ upc = "check" /\ opLock = "none"
  /\ IF CheckDirty /\ Dirty(UOverlay)
       THEN UFail("refused")
       ELSE upc' = "lock" /\ UNCHANGED <<uResult, lease>>
  /\ UNCHANGED <<disk, guest, inst, uOld, uNew, uTodo,
                 opLock, up, crashes, failures, mounts, recovers>>

\* uninstall: lock, working empty check, tip, records, affected_depots.
ULock ==
  /\ upc = "lock" /\ opLock = "none"
  /\ CASE UOverlay # {} \/ UMeta ->
            UFail("refused") /\ UNCHANGED <<opLock, uOld>>
       [] ~trees[ref].recA ->
            UFail("ok") /\ UNCHANGED <<opLock, uOld>>
       [] OTHER ->
            /\ opLock' = "uninstall" /\ uOld' = ref /\ upc' = "rewrite"
            /\ UNCHANGED <<uResult, lease>>
  /\ UNCHANGED <<disk, guest, inst, uNew, uTodo, up, crashes, failures, mounts, recovers>>

\* rewrite_without: rebased commits written as objects (abstracted: drop A).
URewrite ==
  /\ upc = "rewrite"
  /\ LET old == trees[uOld]
         t == Tree([p \in Paths |-> IF p = "app" THEN Absent ELSE old.files[p]], FALSE, old.recB)
     IN /\ trees' = Append(trees, t) /\ uNew' = Len(trees) + 1
  /\ upc' = "cas"
  /\ UNCHANGED <<checkout, idx, ref, reflog, guest, inst, uOld, uTodo, uResult, ctl>>

URewriteFails ==
  /\ upc = "rewrite" /\ failures < MaxFailures
  /\ failures' = failures + 1 /\ opLock' = "none" /\ UFail("error")
  /\ UNCHANGED <<disk, guest, inst, uOld, uNew, uTodo, up, crashes, mounts, recovers>>

\* rewrite_without: edit_references_as MustExistAndMatch(old_tip).
UCas ==
  /\ upc = "cas"
  /\ IF ref = uOld
       THEN /\ ref' = uNew /\ reflog' = reflog \cup {uNew} /\ upc' = "reflog"
            /\ UNCHANGED <<uResult, lease, opLock>>
       ELSE /\ UFail("error") /\ opLock' = "none" /\ UNCHANGED <<ref, reflog>>
  /\ UNCHANGED <<checkout, idx, trees, guest, inst, uOld, uNew, uTodo,
                 up, crashes, failures, mounts, recovers>>

\* rewrite_without: Change::Delete RefLog::Only.
UReflog ==
  /\ upc = "reflog"
  /\ reflog' = {} /\ uTodo' = Paths /\ upc' = "checkout"
  /\ UNCHANGED <<checkout, idx, trees, ref, guest, inst, uOld, uNew, uResult, ctl>>

\* refresh_checkout_locked -> checkout_target / prune_checkout, one path.
UCheckoutPath(p) ==
  /\ upc = "checkout" /\ p \in uTodo
  /\ checkout' = [checkout EXCEPT ![p] = TargetPath(trees[uNew], p)]
  /\ uTodo' = uTodo \ {p}
  /\ UNCHANGED <<idx, trees, ref, reflog, guest, inst, upc, uOld, uNew, uResult, ctl>>

UCheckoutDone ==
  /\ upc = "checkout" /\ uTodo = {}
  /\ upc' = "index"
  /\ UNCHANGED <<disk, guest, inst, uOld, uNew, uTodo, uResult, ctl>>

\* refresh_checkout_locked -> write_index, then the guard drops.
UWriteIndex ==
  /\ upc = "index"
  /\ IF \E p \in Paths : trees[uNew].files[p] # Absent /\ checkout[p] = Absent
       THEN UFail("error") /\ UNCHANGED idx
       ELSE idx' = Stamp(trees[uNew]) /\ UFail("ok")
  /\ opLock' = "none"
  /\ UNCHANGED <<checkout, trees, ref, reflog, guest, inst, uOld, uNew, uTodo,
                 up, crashes, failures, mounts, recovers>>

----------------------------------------------------------------------------

Crash ==
  /\ up /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1 /\ up' = FALSE
  /\ opLock' = "none" /\ lease' = "none"
  /\ mounted' = FALSE /\ opened' = {} /\ gWorking' = {} /\ gpc' = "idle"
  /\ iWorking' = {} /\ iMeta' = FALSE
  /\ ipc' = IF ipc \in {"idle", "done"} THEN ipc ELSE "done"
  /\ iResult' = IF ipc \in {"idle", "done"} THEN iResult ELSE "crashed"
  /\ upc' = IF upc \in {"idle", "done"} THEN upc ELSE "done"
  /\ uResult' = IF upc \in {"idle", "done"} THEN uResult ELSE "crashed"
  /\ UNCHANGED <<disk, gArg, gVal, gAck, nextVal, iNew, uOld, uNew, uTodo,
                 failures, mounts, recovers>>

\* Reopen: GitVolumeStorage::new + ensure_checkout (NAND with a tip: no repair).
Reopen ==
  /\ ~up /\ up' = TRUE
  /\ UNCHANGED <<disk, guest, inst, unin, opLock, lease, crashes, failures, mounts, recovers>>

\* Recover handler: lease, VolumeStorage::recover discards uncommitted changes.
Recover ==
  /\ up /\ recovers < MaxRecovers /\ opLock = "none" /\ ~mounted
  /\ UseLease => lease = "none"
  /\ Dirty({})
  /\ LET t == trees[ref]
         c == [p \in Paths |-> TargetPath(t, p)]
     IN /\ checkout' = c
        /\ idx' = [p \in Paths |-> IF t.files[p] = Absent THEN NoEntry
                                   ELSE [id |-> t.files[p], stat |-> c[p]]]
  /\ gAck' = [p \in GuestPaths |-> Absent]
  /\ recovers' = recovers + 1
  /\ UNCHANGED <<trees, ref, reflog, mounted, opened, gWorking, gpc, gArg, gVal, nextVal,
                 inst, unin, opLock, lease, up, crashes, failures, mounts>>

Finished ==
  /\ ipc = "done" /\ upc = "done" /\ ~mounted /\ up
  /\ UNCHANGED vars

GuestStep == \E p \in GuestPaths : GuestCreate(p) \/ GuestWriteAt(p)

InstallStep == IStart \/ IDirtyCheck \/ IInstallFile \/ IInstallOverlay \/ IRecord
               \/ ICommit \/ ICommitIndex

UninstallStep == UStart \/ UDirtyCheck \/ ULock \/ URewrite \/ UCas \/ UReflog
                 \/ (\E p \in Paths : UCheckoutPath(p)) \/ UCheckoutDone \/ UWriteIndex

Next ==
  \/ Mount \/ GuestStep \/ GuestCreateOverlay \/ Unmount
  \/ InstallStep
  \/ UninstallStep \/ URewriteFails
  \/ Crash \/ Reopen \/ Recover
  \/ Finished

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(InstallStep) /\ WF_vars(UninstallStep)
  /\ WF_vars(GuestCreateOverlay) /\ WF_vars(Unmount) /\ WF_vars(Reopen)

----------------------------------------------------------------------------

Committed(p, v) == trees[ref].files[p] = v
Overlays == gWorking \cup iWorking

\* Every acknowledged guest write is committed or still in the checkout.
NoAckedWriteErased ==
  \A p \in GuestPaths : gAck[p] # Absent => Committed(p, gAck[p]) \/ checkout[p] = gAck[p]

\* Every acknowledged guest write is committed, or in the checkout and listed in a
\* live overlay so that the next commit_pending picks it up.
NoAckedWriteLost ==
  \A p \in GuestPaths : gAck[p] # Absent =>
    \/ Committed(p, gAck[p])
    \/ checkout[p] = gAck[p] /\ p \in Overlays

\* Whenever is_dirty can run, it reports clean only if the checkout equals the ref.
CheckoutMatchesOrDetected ==
  opLock = "none" => (CheckoutMatches(trees[ref]) => checkout = trees[ref].files)

\* No overlay entry names a file missing from the checkout.
OverlayBacked == \A p \in Overlays : checkout[p] # Absent

\* An artifact record and its files are in the ref together.
RecordConsistent ==
  /\ trees[ref].recB <=> trees[ref].files["inst"] # Absent
  /\ trees[ref].recA <=> trees[ref].files["app"] # Absent

RecordDurable == iResult = "ok" => trees[ref].recB

UninstallEffective == uResult = "ok" => ~trees[ref].recA

\* Once A is uninstalled, no reflog entry still reaches it.
ReflogScrubbed ==
  opLock = "none" /\ ~trees[ref].recA => \A c \in reflog : ~trees[c].recA

UninstallTerminates == upc \notin {"idle", "done"} ~> upc = "done"

InstallTerminates == ipc \notin {"idle", "done"} ~> ipc = "done"

=============================================================================
