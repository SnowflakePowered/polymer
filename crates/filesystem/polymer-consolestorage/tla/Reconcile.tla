----------------------------- MODULE Reconcile -----------------------------
(***************************************************************************)
(* T4: memory-card writeback through ReconcileStorage into git-backed     *)
(* save history.                                                          *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   mem        MemoryCardStorage.file (src/storage/memory/file.rs MemoryFile,*)
(*              one Mutex: write_at and bytes() are atomic); value = how   *)
(*              many guest writes the image contains                       *)
(*   gpc, gver  one guest thread inside ReconcileFile::write_at / sync     *)
(*              (src/storage/reconcile/file.rs)                              *)
(*   sv         acked when that thread called ReconcileFile::sync          *)
(*   acked      newest write whose write_at returned Ok to the guest       *)
(*   synced     newest write covered by a sync that returned Ok            *)
(*   flag       ReconcileWorker.dirty_enqueued (src/storage/reconcile/worker.rs)               *)
(*   queue      ReconcileWorker.commands mpsc channel                      *)
(*   dirty      local `dirty` in ReconcileWorker::run                      *)
(*   wpc        ReconcileWorker::run loop / reconcile closure progress;    *)
(*              "stopped" = thread exited, receiver dropped                *)
(*   wcause     the command (or timeout) that triggered reconcile()        *)
(*   snap       bytes taken by MemoryCardStorage::reconcile (src/impls/memorycard/raw.rs)     *)
(*   disk       save file in the GitVolumeStorage checkout, written by     *)
(*              src/impls/ps2/mod.rs write_save (create truncates, then          *)
(*              write_all_at); Truncated = after create, before write      *)
(*   notified   GitVolumeInner.changes Notify permit (notify_one)          *)
(*   hpc, hexp, hfinal, hstop                                              *)
(*              polymer-save-history thread in SaveWatchFactory            *)
(*              (polymer-runtime/src/storage.rs): checkpoint_saves =       *)
(*              export_save (read disk) then commit on refs(depot).head    *)
(*   commits    versions held by save commits (history keeps all)         *)
(*   phase, epc MaterializedResources::finalize: MemoryCardLease.finalize  *)
(*              -> ReconcileStorage::finish -> ReconcileWorker::shutdown,  *)
(*              then SaveWatchingLease::stop (final checkpoint_saves)      *)
(*   Crash      process death; Recover = next session's baseline           *)
(*              checkpoint_saves in SaveWatchFactory::materialize          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Threads, MaxWrites, MaxFailures, MaxCrashes, InFlightAtExit

ASSUME InFlightAtExit \in BOOLEAN

Truncated == -1
NoSender == "none"
Msg(k, f) == [kind |-> k, from |-> f]
Max(a, b) == IF a >= b THEN a ELSE b
MaxOf(S) == CHOOSE x \in S : \A y \in S : x >= y

VARIABLES mem, nextVer, gpc, gver, sv, acked, synced,
          flag, queue, dirty, wpc, wcause, snap,
          disk, notified, hpc, hexp, hfinal, hstop, commits,
          phase, epc, failures, crashes

guest   == <<mem, nextVer, gpc, gver, sv, acked, synced>>
worker  == <<flag, queue, dirty, wpc, wcause, snap>>
history == <<disk, notified, hpc, hexp, hfinal, hstop, commits>>
control == <<phase, epc, failures, crashes>>
vars == <<guest, worker, history, control>>

TypeOK ==
  /\ mem \in 0..MaxWrites /\ nextVer \in 1..MaxWrites + 1
  /\ gpc \in [Threads -> {"idle", "swap", "send", "syncWait", "syncOk"}]
  /\ disk \in {Truncated} \cup 0..MaxWrites
  /\ wpc \in {"recv", "snap", "create", "write", "stopped"}
  /\ hpc \in {"wait", "export", "commit", "stopped"}
  /\ phase \in {"running", "exiting", "done", "recovering", "recovered"}
  /\ epc \in {"none", "shutdown", "shutdownWait", "historyWait"}

Init ==
  /\ mem = 0 /\ nextVer = 1
  /\ gpc = [t \in Threads |-> "idle"] /\ gver = [t \in Threads |-> 0]
  /\ sv = [t \in Threads |-> 0] /\ acked = 0 /\ synced = 0
  /\ flag = FALSE /\ queue = <<>> /\ dirty = FALSE /\ wpc = "recv"
  /\ wcause = Msg("timeout", NoSender) /\ snap = 0
  /\ disk = 0 /\ notified = FALSE /\ hpc = "wait" /\ hexp = 0
  /\ hfinal = FALSE /\ hstop = FALSE /\ commits = {0}
  /\ phase = "running" /\ epc = "none" /\ failures = 0 /\ crashes = 0

Alive == wpc # "stopped"

Send(m) == queue' = IF Alive THEN Append(queue, m) ELSE queue

----------------------------------------------------------------------------
\* ReconcileFile::write_at: MemoryFile::write_at, then ReconcileWorker::schedule.
WriteAt(t) ==
  /\ phase = "running" /\ gpc[t] = "idle" /\ nextVer <= MaxWrites
  /\ mem' = nextVer /\ gver' = [gver EXCEPT ![t] = nextVer]
  /\ nextVer' = nextVer + 1
  /\ gpc' = [gpc EXCEPT ![t] = "swap"]
  /\ UNCHANGED <<sv, acked, synced, worker, history, control>>

\* schedule: dirty_enqueued.swap(true); a true result returns Ok immediately.
ScheduleSwap(t) ==
  /\ gpc[t] = "swap"
  /\ flag' = TRUE
  /\ IF flag
       THEN /\ gpc' = [gpc EXCEPT ![t] = "idle"] /\ acked' = Max(acked, gver[t])
       ELSE /\ gpc' = [gpc EXCEPT ![t] = "send"] /\ UNCHANGED acked
  /\ UNCHANGED <<mem, nextVer, gver, sv, synced, queue, dirty, wpc, wcause, snap,
                 history, control>>

\* schedule: commands.send(Dirty); a dropped receiver is ignored.
ScheduleSend(t) ==
  /\ gpc[t] = "send"
  /\ Send(Msg("dirty", NoSender))
  /\ gpc' = [gpc EXCEPT ![t] = "idle"] /\ acked' = Max(acked, gver[t])
  /\ UNCHANGED <<mem, nextVer, gver, sv, synced, flag, dirty, wpc, wcause, snap,
                 history, control>>

\* ReconcileFile::sync -> ReconcileWorker::reconcile: send Sync, block on reply.
Sync(t) ==
  /\ phase = "running" /\ gpc[t] = "idle"
  /\ IF Alive
       THEN /\ queue' = Append(queue, Msg("sync", t))
            /\ gpc' = [gpc EXCEPT ![t] = "syncWait"]
            /\ sv' = [sv EXCEPT ![t] = acked]
       ELSE UNCHANGED <<queue, gpc, sv>>
  /\ UNCHANGED <<mem, nextVer, gver, acked, synced, flag, dirty, wpc, wcause, snap,
                 history, control>>

SyncReturn(t) ==
  /\ gpc[t] = "syncOk"
  /\ synced' = Max(synced, sv[t])
  /\ gpc' = [gpc EXCEPT ![t] = "idle"]
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, worker, history, control>>

\* The reply sender was dropped with the queue when the worker exited.
SyncAbandoned(t) ==
  /\ gpc[t] = "syncWait" /\ ~Alive
  /\ gpc' = [gpc EXCEPT ![t] = "idle"]
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, synced, worker, history, control>>

----------------------------------------------------------------------------
\* ReconcileWorker::run

Reply(m, ok) ==
  IF m.kind = "sync"
    THEN gpc' = [gpc EXCEPT ![m.from] = IF ok THEN "syncOk" ELSE "idle"]
    ELSE UNCHANGED gpc

StartReconcile(m) == /\ wpc' = "snap" /\ wcause' = m

RecvDirty ==
  /\ wpc = "recv" /\ queue # <<>> /\ Head(queue).kind = "dirty"
  /\ queue' = Tail(queue) /\ flag' = FALSE /\ dirty' = TRUE
  /\ UNCHANGED <<guest, wpc, wcause, snap, history, control>>

RecvSync ==
  /\ wpc = "recv" /\ queue # <<>> /\ Head(queue).kind = "sync"
  /\ queue' = Tail(queue)
  /\ IF dirty
       THEN StartReconcile(Head(queue)) /\ UNCHANGED gpc
       ELSE Reply(Head(queue), TRUE) /\ UNCHANGED <<wpc, wcause>>
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, synced, flag, dirty, snap, history, control>>

RecvShutdown ==
  /\ wpc = "recv" /\ queue # <<>> /\ Head(queue).kind = "shutdown"
  /\ queue' = Tail(queue)
  /\ IF dirty
       THEN StartReconcile(Head(queue))
       ELSE wpc' = "stopped" /\ UNCHANGED wcause
  /\ UNCHANGED <<guest, flag, dirty, snap, history, control>>

\* recv_timeout drains pending commands before reporting Timeout.
Timeout ==
  /\ wpc = "recv" /\ queue = <<>> /\ dirty
  /\ StartReconcile(Msg("timeout", NoSender))
  /\ UNCHANGED <<guest, flag, queue, dirty, snap, history, control>>

\* MemoryCardStorage::reconcile: self.file.bytes().
Snapshot ==
  /\ wpc = "snap"
  /\ snap' = mem /\ wpc' = "create"
  /\ UNCHANGED <<guest, flag, queue, dirty, wcause, history, control>>

\* write_save: storage.create(path) truncates the checkout file and notifies.
Create ==
  /\ wpc = "create"
  /\ disk' = Truncated /\ notified' = TRUE /\ wpc' = "write"
  /\ UNCHANGED <<guest, flag, queue, dirty, wcause, snap, hpc, hexp, hfinal, hstop,
                 commits, control>>

Finish(ok) ==
  /\ IF wcause.kind = "shutdown"
       THEN wpc' = "stopped" /\ UNCHANGED dirty
       ELSE wpc' = "recv" /\ dirty' = ~ok
  /\ Reply(wcause, ok)

\* write_all_at + set_len + sync, then reconcile() returns Ok.
WriteBytes ==
  /\ wpc = "write"
  /\ disk' = snap /\ notified' = TRUE
  /\ Finish(TRUE)
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, synced, flag, queue, wcause, snap,
                 hpc, hexp, hfinal, hstop, commits, control>>

ReconcileFails ==
  /\ wpc \in {"snap", "create", "write"} /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ Finish(FALSE)
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, synced, flag, queue, wcause, snap,
                 history, phase, epc, crashes>>

----------------------------------------------------------------------------
\* polymer-save-history: select! { stopped => final checkpoint, changed => checkpoint }

WakeChanged ==
  /\ hpc = "wait" /\ notified
  /\ notified' = FALSE /\ hpc' = "export" /\ hfinal' = FALSE
  /\ UNCHANGED <<guest, worker, disk, hexp, hstop, commits, control>>

WakeStopped ==
  /\ hpc = "wait" /\ hstop
  /\ hpc' = "export" /\ hfinal' = TRUE
  /\ UNCHANGED <<guest, worker, disk, notified, hexp, hstop, commits, control>>

\* checkpoint: owner.export_save reads the checkout.
Export ==
  /\ hpc = "export"
  /\ hexp' = disk /\ hpc' = "commit"
  /\ UNCHANGED <<guest, worker, disk, notified, hfinal, hstop, commits, control>>

\* checkpoint: repo.commit_as on refs(depot).head.
Commit ==
  /\ hpc = "commit"
  /\ commits' = commits \cup {hexp}
  /\ hpc' = IF hfinal THEN "stopped" ELSE "wait"
  /\ UNCHANGED <<guest, worker, disk, notified, hexp, hfinal, hstop, control>>

CheckpointFails ==
  /\ hpc \in {"export", "commit"} /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ hpc' = IF hfinal THEN "stopped" ELSE "wait"
  /\ UNCHANGED <<guest, worker, disk, notified, hexp, hfinal, hstop, commits,
                 phase, epc, crashes>>

----------------------------------------------------------------------------
\* Emulator process exit, then MaterializedResources::finalize (leases in reverse).

EmulatorExit ==
  /\ phase = "running"
  /\ InFlightAtExit \/ \A t \in Threads : gpc[t] = "idle"
  /\ phase' = "exiting" /\ epc' = "shutdown"
  /\ UNCHANGED <<guest, worker, history, failures, crashes>>

\* ReconcileStorage::finish -> ReconcileWorker::shutdown: send Shutdown.
ShutdownSend ==
  /\ epc = "shutdown"
  /\ Send(Msg("shutdown", NoSender))
  /\ epc' = "shutdownWait"
  /\ UNCHANGED <<guest, flag, dirty, wpc, wcause, snap, history, phase, failures, crashes>>

\* recv the reply, join the thread, then SaveWatchingLease::stop.
ShutdownJoin ==
  /\ epc = "shutdownWait" /\ ~Alive
  /\ hstop' = TRUE /\ epc' = "historyWait"
  /\ UNCHANGED <<guest, worker, disk, notified, hpc, hexp, hfinal, commits,
                 phase, failures, crashes>>

HistoryJoin ==
  /\ epc = "historyWait" /\ hpc = "stopped"
  /\ phase' = "done" /\ epc' = "none"
  /\ UNCHANGED <<guest, worker, history, failures, crashes>>

----------------------------------------------------------------------------

Crash ==
  /\ phase \in {"running", "exiting"} /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ gpc' = [t \in Threads |-> "idle"]
  /\ flag' = FALSE /\ queue' = <<>> /\ dirty' = FALSE /\ wpc' = "stopped"
  /\ notified' = FALSE /\ hpc' = "stopped" /\ hstop' = FALSE
  /\ phase' = "recovering" /\ epc' = "none"
  /\ UNCHANGED <<mem, nextVer, gver, sv, acked, synced, wcause, snap,
                 disk, hexp, hfinal, commits, failures>>

\* Next session: SaveWatchFactory::materialize runs a baseline checkpoint_saves.
Recover ==
  /\ phase = "recovering"
  /\ commits' = commits \cup {disk}
  /\ phase' = "recovered"
  /\ UNCHANGED <<guest, worker, disk, notified, hpc, hexp, hfinal, hstop,
                 epc, failures, crashes>>

Finished ==
  /\ phase \in {"done", "recovered"}
  /\ UNCHANGED vars

InFlight(t) == ScheduleSwap(t) \/ ScheduleSend(t) \/ SyncReturn(t) \/ SyncAbandoned(t)

WorkerStep == RecvDirty \/ RecvSync \/ RecvShutdown \/ Timeout \/ Snapshot \/ Create \/ WriteBytes

HistoryStep == WakeChanged \/ WakeStopped \/ Export \/ Commit

ExitStep == ShutdownSend \/ ShutdownJoin \/ HistoryJoin \/ Recover

Next ==
  \/ \E t \in Threads : WriteAt(t) \/ Sync(t) \/ InFlight(t)
  \/ WorkerStep \/ ReconcileFails
  \/ HistoryStep \/ CheckpointFails
  \/ EmulatorExit \/ ExitStep
  \/ Crash
  \/ Finished

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A t \in Threads : WF_vars(InFlight(t))
  /\ WF_vars(WorkerStep) /\ WF_vars(HistoryStep) /\ WF_vars(ExitStep)

----------------------------------------------------------------------------

Committed == MaxOf(commits)

\* A sync that returns Ok has put every write acknowledged before it on disk.
SyncReturnsDurable ==
  [][\A t \in Threads : (gpc[t] = "syncWait" /\ gpc'[t] = "syncOk") => disk' >= sv[t]]_vars

\* Clean exit: every acknowledged write is in a save commit or in the checkout.
CleanExitPersistsAcked == phase = "done" => Committed >= acked \/ disk >= acked

\* Clean exit: the final checkpoint committed every acknowledged write.
CleanExitCommitsAcked == phase = "done" => Committed >= acked

\* After a crash and the next baseline, every synced write is committed.
RecoveredKeepsSynced == phase = "recovered" => Committed >= synced

\* After a crash and the next baseline, every acknowledged write is committed.
RecoveredKeepsAcked == phase = "recovered" => Committed >= acked

ExitTerminates == phase = "exiting" ~> phase \in {"done", "recovered"}

=============================================================================
