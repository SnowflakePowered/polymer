----------------------------- MODULE JobStore -----------------------------
(***************************************************************************)
(* T8: durable job store, job runner and import jobs.                     *)
(* Also models polymer-runtime/src/jobs/ (JobActor handlers for           *)
(* JobsImport / JobsCancel / JobsRetry, JobActor::started, and            *)
(* Importer::run / Importer::run_job).  All jobs are Import jobs for one  *)
(* game, i.e. they share one (kind, dedupe = path) key.                   *)
(*                                                                         *)
(* Rust mapping (polymer-library/src/jobs/store.rs, runner.rs, handle.rs): *)
(*   state[j]     jobs.state column of job j ("none" = row not inserted)   *)
(*   pending[j]   Importer::run spawned run_job(j) tasks not yet past      *)
(*                JobStore::claim (volatile)                               *)
(*   working[j]   run_job(j) tasks holding a JobHandle, inside            *)
(*                Importer::execute (volatile)                             *)
(*   opPc, opJob  an in-flight JobsCancel / JobsRetry handler between its  *)
(*                unlocked get() check and its modify() (volatile)         *)
(*   boot         JobActor::started's spawned resume(&[Import]):           *)
(*                "recover" = before recover()'s UPDATE running->queued,   *)
(*                "list" = before recover()'s list() + runner.run(resume), *)
(*                "up" = done                                              *)
(*   settled[j]   ghost: terminal transitions since j was last queued      *)
(*                                                                         *)
(* Actions:                                                                *)
(*   Submit       JobStore::submit = enqueue (one IMMEDIATE txn under      *)
(*                `writes`: active-dedupe SELECT + INSERT) + runner.run    *)
(*   CancelCheck  JobStore::cancel: get() + Queued|Paused check (no lock)  *)
(*   CancelModify JobStore::cancel: modify(state = Cancelled)              *)
(*   RetryCheck   JobStore::retry -> requeue: get(), !active, active()     *)
(*   RetryModify  requeue's modify(state = Queued) (rejected by the        *)
(*                jobs_active unique index if another row is active)       *)
(*                + runner.run(resume)                                     *)
(*   Claim        JobStore::claim: UPDATE ... WHERE state = 'queued'       *)
(*   Finish       JobHandle::settle -> finish: modify(state = outcome)     *)
(*   FinishFails  finish sets `finished` before its modify() fails, so     *)
(*                Drop does not fail the job; run_job returns Err          *)
(*   RecoverUpdate JobStore::recover's UPDATE running -> queued            *)
(*   RecoverList  recover's list() of queued jobs + runner.run(ids, true)  *)
(*   Crash        process death: tasks and handlers vanish, rows persist   *)
(*                                                                         *)
(* Abstracted: Importer::execute (identify, child install jobs, steps,     *)
(* detail, the `identifying` watch) is the single working phase whose      *)
(* outcome is Completed or Failed; progress/step/record writes never touch *)
(* state.  Paused and Partial are never produced for imports.  Ephemeral   *)
(* jobs, dismiss and abandon(Download) are not modelled.                   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Jobs, Clients, MaxOps, MaxCrashes, MaxFailures, OpsDuringRecovery

ASSUME OpsDuringRecovery \in BOOLEAN

Active == {"queued", "running"}
Terminal == {"completed", "failed", "cancelled"}
States == {"none"} \cup Active \cup Terminal

VARIABLES state, pending, working, opPc, opJob, boot, settled,
          ops, crashes, failures

vars == <<state, pending, working, opPc, opJob, boot, settled, ops, crashes, failures>>

TypeOK ==
  /\ state \in [Jobs -> States]
  /\ pending \in [Jobs -> Nat] /\ working \in [Jobs -> Nat]
  /\ opPc \in [Clients -> {"idle", "cancel", "retry"}]
  /\ opJob \in [Clients -> Jobs]
  /\ boot \in {"recover", "list", "up"}
  /\ settled \in [Jobs -> Nat]

Init ==
  /\ state = [j \in Jobs |-> "none"]
  /\ pending = [j \in Jobs |-> 0]
  /\ working = [j \in Jobs |-> 0]
  /\ opPc = [c \in Clients |-> "idle"]
  /\ opJob = [c \in Clients |-> CHOOSE j \in Jobs : TRUE]
  /\ boot = "recover"
  /\ settled = [j \in Jobs |-> 0]
  /\ ops = 0 /\ crashes = 0 /\ failures = 0

AnyActive == \E k \in Jobs : state[k] \in Active

\* settled counts entries into a terminal state and changes between terminal states.
Write(j, s) ==
  /\ state' = [state EXCEPT ![j] = s]
  /\ settled' = [settled EXCEPT ![j] =
       IF s \in Terminal /\ s # state[j] THEN @ + 1
       ELSE IF s = "queued" /\ state[j] \in Terminal THEN 0 ELSE @]

StartOp(c) ==
  /\ opPc[c] = "idle" /\ ops < MaxOps
  /\ (OpsDuringRecovery \/ boot = "up")
  /\ ops' = ops + 1

Submit(c) ==
  /\ StartOp(c)
  /\ IF AnyActive \/ \A j \in Jobs : state[j] # "none"
       THEN UNCHANGED <<state, pending, settled>>
       ELSE \E j \in Jobs :
              /\ state[j] = "none"
              /\ \A k \in Jobs : state[k] = "none" => k >= j
              /\ Write(j, "queued")
              /\ pending' = [pending EXCEPT ![j] = @ + 1]
  /\ UNCHANGED <<working, opPc, opJob, boot, crashes, failures>>

CancelCheck(c, j) ==
  /\ StartOp(c)
  /\ state[j] = "queued"
  /\ opPc' = [opPc EXCEPT ![c] = "cancel"] /\ opJob' = [opJob EXCEPT ![c] = j]
  /\ UNCHANGED <<state, pending, working, boot, settled, crashes, failures>>

CancelModify(c) ==
  /\ opPc[c] = "cancel"
  /\ Write(opJob[c], "cancelled")
  /\ opPc' = [opPc EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<pending, working, opJob, boot, ops, crashes, failures>>

RetryCheck(c, j) ==
  /\ StartOp(c)
  /\ state[j] \in Terminal /\ ~AnyActive
  /\ opPc' = [opPc EXCEPT ![c] = "retry"] /\ opJob' = [opJob EXCEPT ![c] = j]
  /\ UNCHANGED <<state, pending, working, boot, settled, crashes, failures>>

RetryModify(c) ==
  LET j == opJob[c] IN
  /\ opPc[c] = "retry"
  /\ IF \E k \in Jobs \ {j} : state[k] \in Active
       THEN UNCHANGED <<state, pending, settled>>
       ELSE /\ Write(j, "queued")
            /\ pending' = [pending EXCEPT ![j] = @ + 1]
  /\ opPc' = [opPc EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<working, opJob, boot, ops, crashes, failures>>

Claim(j) ==
  /\ pending[j] > 0
  /\ pending' = [pending EXCEPT ![j] = @ - 1]
  /\ IF state[j] = "queued"
       THEN /\ Write(j, "running")
            /\ working' = [working EXCEPT ![j] = @ + 1]
       ELSE UNCHANGED <<state, settled, working>>
  /\ UNCHANGED <<opPc, opJob, boot, ops, crashes, failures>>

Finish(j) ==
  /\ working[j] > 0
  /\ working' = [working EXCEPT ![j] = @ - 1]
  /\ \E s \in {"completed", "failed"} : Write(j, s)
  /\ UNCHANGED <<pending, opPc, opJob, boot, ops, crashes, failures>>

FinishFails(j) ==
  /\ working[j] > 0 /\ failures < MaxFailures
  /\ working' = [working EXCEPT ![j] = @ - 1]
  /\ failures' = failures + 1
  /\ UNCHANGED <<state, pending, opPc, opJob, boot, settled, ops, crashes>>

RecoverUpdate ==
  /\ boot = "recover"
  /\ state' = [j \in Jobs |-> IF state[j] = "running" THEN "queued" ELSE state[j]]
  /\ boot' = "list"
  /\ UNCHANGED <<pending, working, opPc, opJob, settled, ops, crashes, failures>>

RecoverList ==
  /\ boot = "list"
  /\ pending' = [j \in Jobs |-> pending[j] + (IF state[j] = "queued" THEN 1 ELSE 0)]
  /\ boot' = "up"
  /\ UNCHANGED <<state, working, opPc, opJob, settled, ops, crashes, failures>>

Crash ==
  /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ pending' = [j \in Jobs |-> 0]
  /\ working' = [j \in Jobs |-> 0]
  /\ opPc' = [c \in Clients |-> "idle"]
  /\ boot' = "recover"
  /\ UNCHANGED <<state, opJob, settled, ops, failures>>

Finished ==
  /\ boot = "up"
  /\ \A j \in Jobs : pending[j] = 0 /\ working[j] = 0
  /\ \A c \in Clients : opPc[c] = "idle"
  /\ UNCHANGED vars

Next ==
  \/ \E c \in Clients : Submit(c) \/ CancelModify(c) \/ RetryModify(c)
                        \/ \E j \in Jobs : CancelCheck(c, j) \/ RetryCheck(c, j)
  \/ \E j \in Jobs : Claim(j) \/ Finish(j) \/ FinishFails(j)
  \/ RecoverUpdate \/ RecoverList \/ Crash \/ Finished

Spec == Init /\ [][Next]_vars
          /\ WF_vars(RecoverUpdate) /\ WF_vars(RecoverList)
          /\ \A j \in Jobs : WF_vars(Claim(j)) /\ WF_vars(Finish(j))
          /\ \A c \in Clients : WF_vars(CancelModify(c)) /\ WF_vars(RetryModify(c))

\* What the jobs_active unique index promises.
AtMostOneActivePerGame ==
  \A j, k \in Jobs : j # k => ~(state[j] \in Active /\ state[k] \in Active)

\* One import of the game executes at a time.
AtMostOneRunner ==
  \A j \in Jobs : working[j] <= 1 /\ \A k \in Jobs \ {j} : working[j] = 0 \/ working[k] = 0

\* A job reaches a terminal state once per (re)queue.
TerminalOnce == \A j \in Jobs : settled[j] <= 1

\* A terminal state is only left by a retry.
TerminalOnlyLeftByRequeue ==
  [][\A j \in Jobs : state[j] \in Terminal /\ state'[j] # state[j] => state'[j] = "queued"]_vars

\* A cancelled job stays cancelled until retried.
CancelSticky ==
  [][\A j \in Jobs : state[j] = "cancelled" => state'[j] \in {"cancelled", "queued"}]_vars

\* No job stays queued or running forever.
NoPermanentlyPending == \A j \in Jobs : state[j] \in Active ~> state[j] \notin Active

=============================================================================
