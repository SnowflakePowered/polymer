----------------------- MODULE ResourceLeaseRetries -----------------------
EXTENDS Integers, Sequences, FiniteSets, TLC
CONSTANTS Leases, MaxSpawnFailures, MaxCleanupFailures, AllowHang, Hangable
VARIABLES status, held, queue, batch, failed, worker, startRequested, spawnFailures, cleanupFailures
vars == <<status, held, queue, batch, failed, worker, startRequested, spawnFailures, cleanupFailures>>
Range(s) == {s[i] : i \in 1..Len(s)}

Init ==
    /\ status = [l \in Leases |-> "external"] /\ held = Leases
    /\ queue = <<>> /\ batch = <<>> /\ failed = <<>>
    /\ worker = "stopped" /\ startRequested = FALSE
    /\ spawnFailures = 0 /\ cleanupFailures = 0

Retain(l) ==
    /\ status[l] = "external" /\ ~startRequested
    /\ status' = [status EXCEPT ![l] = "queued"]
    /\ queue' = Append(queue, l)
    /\ startRequested' = (worker = "stopped")
    /\ UNCHANGED <<held, batch, failed, worker, spawnFailures, cleanupFailures>>

StartWorker ==
    /\ startRequested /\ worker = "stopped"
    /\ worker' = "idle" /\ startRequested' = FALSE
    /\ UNCHANGED <<status, held, queue, batch, failed, spawnFailures, cleanupFailures>>

SpawnFailure ==
    /\ startRequested /\ worker = "stopped" /\ spawnFailures < MaxSpawnFailures
    /\ startRequested' = FALSE /\ spawnFailures' = spawnFailures + 1
    /\ UNCHANGED <<status, held, queue, batch, failed, worker, cleanupFailures>>

TakeBatch ==
    /\ worker = "idle" /\ queue # <<>>
    /\ batch' = queue /\ queue' = <<>> /\ worker' = "busy"
    /\ status' = [l \in Leases |-> IF l \in Range(queue) THEN "batch" ELSE status[l]]
    /\ UNCHANGED <<held, failed, startRequested, spawnFailures, cleanupFailures>>

FinishHead ==
    /\ worker = "busy" /\ batch # <<>>
    /\ status' = [status EXCEPT ![Head(batch)] = "done"]
    /\ held' = held \ {Head(batch)} /\ batch' = Tail(batch)
    /\ UNCHANGED <<queue, failed, worker, startRequested, spawnFailures, cleanupFailures>>

FailHead ==
    /\ worker = "busy" /\ batch # <<>> /\ cleanupFailures < MaxCleanupFailures
    /\ status' = [status EXCEPT ![Head(batch)] = "retry"]
    /\ failed' = Append(failed, Head(batch)) /\ batch' = Tail(batch)
    /\ cleanupFailures' = cleanupFailures + 1
    /\ UNCHANGED <<held, queue, worker, startRequested, spawnFailures>>

HangHead ==
    /\ AllowHang /\ worker = "busy" /\ batch # <<>>
    /\ Head(batch) \in Hangable
    /\ worker' = "hung"
    /\ UNCHANGED <<status, held, queue, batch, failed, startRequested, spawnFailures, cleanupFailures>>

EndBatch ==
    /\ worker = "busy" /\ batch = <<>>
    /\ queue' = queue \o failed /\ failed' = <<>> /\ worker' = "idle"
    /\ status' = [l \in Leases |-> IF l \in Range(failed) THEN "queued" ELSE status[l]]
    /\ UNCHANGED <<held, batch, startRequested, spawnFailures, cleanupFailures>>

WorkerStep == TakeBatch \/ FinishHead \/ FailHead \/ HangHead \/ EndBatch
Next == (\E l \in Leases : Retain(l)) \/ StartWorker \/ SpawnFailure \/ WorkerStep \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars /\ (\A l \in Leases : WF_vars(Retain(l)))
    /\ WF_vars(StartWorker) /\ WF_vars(WorkerStep)

TypeOK ==
    /\ status \in [Leases -> {"external", "queued", "batch", "retry", "done"}]
    /\ held \subseteq Leases /\ queue \in Seq(Leases) /\ batch \in Seq(Leases) /\ failed \in Seq(Leases)
    /\ worker \in {"stopped", "idle", "busy", "hung"} /\ startRequested \in BOOLEAN
    /\ spawnFailures \in 0..MaxSpawnFailures /\ cleanupFailures \in 0..MaxCleanupFailures
RetainedProtected == \A l \in Leases : (l \in held) = (status[l] # "done")
QueueAccountsForRetained ==
    /\ Range(queue) = {l \in Leases : status[l] = "queued"}
    /\ Range(batch) = {l \in Leases : status[l] = "batch"}
    /\ Range(failed) = {l \in Leases : status[l] = "retry"}
    /\ Len(queue) + Len(batch) + Len(failed) = Cardinality(Range(queue) \cup Range(batch) \cup Range(failed))
AllReleased == <>(held = {})
ReleaseWithAvailableWorker == (<>[](worker \in {"idle", "busy"})) => AllReleased
UnhangingLeasesReleased == <>(held \ Hangable = {})
=============================================================================
