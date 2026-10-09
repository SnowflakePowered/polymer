----------------------------- MODULE JobStore -----------------------------
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Jobs, Clients, MaxOps, MaxCrashes, MaxFailures

Active == {"queued", "running"}
Terminal == {"completed", "failed", "cancelled"}
States == {"none"} \cup Active \cup Terminal

VARIABLES state, pending, working, opPc, opJob, boot, settled,
          ops, crashes, failures, cleanup

vars == <<state, pending, working, opPc, opJob, boot, settled, ops, crashes, failures, cleanup>>

TypeOK ==
  /\ cleanup \subseteq Jobs
  /\ state \in [Jobs -> States]
  /\ pending \in [Jobs -> Nat] /\ working \in [Jobs -> Nat]
  /\ opPc \in [Clients -> {"idle", "cancel", "retry"}]
  /\ opJob \in [Clients -> Jobs]
  /\ boot \in {"recover", "list", "up"}
  /\ settled \in [Jobs -> Nat]

Init ==
  /\ cleanup = {}
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
  /\ boot # "recover"
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
  /\ UNCHANGED <<working, opPc, opJob, boot, crashes, failures, cleanup>>

CancelCheck(c, j) ==
  /\ StartOp(c)
  /\ state[j] = "queued"
  /\ opPc' = [opPc EXCEPT ![c] = "cancel"] /\ opJob' = [opJob EXCEPT ![c] = j]
  /\ UNCHANGED <<state, pending, working, boot, settled, crashes, failures, cleanup>>

CancelModify(c) ==
  /\ opPc[c] = "cancel"
  /\ IF state[opJob[c]] = "queued"
       THEN Write(opJob[c], "cancelled")
       ELSE UNCHANGED <<state, settled>>
  /\ opPc' = [opPc EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<pending, working, opJob, boot, ops, crashes, failures, cleanup>>

RetryCheck(c, j) ==
  /\ StartOp(c)
  /\ state[j] \in Terminal /\ ~AnyActive
  /\ opPc' = [opPc EXCEPT ![c] = "retry"] /\ opJob' = [opJob EXCEPT ![c] = j]
  /\ UNCHANGED <<state, pending, working, boot, settled, crashes, failures, cleanup>>

RetryModify(c) ==
  LET j == opJob[c] IN
  /\ opPc[c] = "retry"
  /\ IF state[j] \notin Terminal \/ AnyActive
       THEN UNCHANGED <<state, pending, settled>>
       ELSE /\ Write(j, "queued")
            /\ pending' = [pending EXCEPT ![j] = @ + 1]
  /\ opPc' = [opPc EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<working, opJob, boot, ops, crashes, failures, cleanup>>

Claim(j) ==
  /\ pending[j] > 0
  /\ pending' = [pending EXCEPT ![j] = @ - 1]
  /\ IF state[j] = "queued"
       THEN /\ Write(j, "running")
            /\ working' = [working EXCEPT ![j] = @ + 1]
       ELSE UNCHANGED <<state, settled, working>>
  /\ UNCHANGED <<opPc, opJob, boot, ops, crashes, failures, cleanup>>

Finish(j) ==
  /\ working[j] > 0 /\ state[j] = "running"
  /\ working' = [working EXCEPT ![j] = @ - 1]
  /\ \E s \in {"completed", "failed"} : Write(j, s)
  /\ UNCHANGED <<pending, opPc, opJob, boot, ops, crashes, failures, cleanup>>

FinishFails(j) ==
  /\ working[j] > 0 /\ failures < MaxFailures
  /\ working' = [working EXCEPT ![j] = @ - 1]
  /\ failures' = failures + 1
  /\ cleanup' = cleanup \cup {j}
  /\ UNCHANGED <<state, pending, opPc, opJob, boot, settled, ops, crashes>>

Cleanup(j) ==
  /\ j \in cleanup
  /\ cleanup' = cleanup \ {j}
  /\ IF state[j] = "running" THEN Write(j, "failed")
       ELSE UNCHANGED <<state, settled>>
  /\ UNCHANGED <<pending, working, opPc, opJob, boot, ops, crashes, failures>>

CleanupFails(j) ==
  /\ j \in cleanup /\ failures < MaxFailures
  /\ cleanup' = cleanup \ {j}
  /\ failures' = failures + 1
  /\ UNCHANGED <<state, pending, working, opPc, opJob, boot, settled, ops, crashes>>

RecoverUpdate ==
  /\ boot = "recover"
  /\ state' = [j \in Jobs |-> IF state[j] = "running" THEN "queued" ELSE state[j]]
  /\ boot' = "list"
  /\ UNCHANGED <<pending, working, opPc, opJob, settled, ops, crashes, failures, cleanup>>

RecoverList ==
  /\ boot = "list"
  /\ pending' = [j \in Jobs |-> pending[j] + (IF state[j] = "queued" THEN 1 ELSE 0)]
  /\ boot' = "up"
  /\ UNCHANGED <<state, working, opPc, opJob, settled, ops, crashes, failures, cleanup>>

Crash ==
  /\ cleanup' = {}
  /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ pending' = [j \in Jobs |-> 0]
  /\ working' = [j \in Jobs |-> 0]
  /\ opPc' = [c \in Clients |-> "idle"]
  /\ boot' = "recover"
  /\ UNCHANGED <<state, opJob, settled, ops, failures>>

Finished ==
  /\ cleanup = {}
  /\ boot = "up"
  /\ \A j \in Jobs : pending[j] = 0 /\ working[j] = 0
  /\ \A c \in Clients : opPc[c] = "idle"
  /\ UNCHANGED vars

Next ==
  \/ \E c \in Clients : Submit(c) \/ CancelModify(c) \/ RetryModify(c)
                        \/ \E j \in Jobs : CancelCheck(c, j) \/ RetryCheck(c, j)
  \/ \E j \in Jobs : Claim(j) \/ Finish(j) \/ FinishFails(j) \/ Cleanup(j) \/ CleanupFails(j)
  \/ RecoverUpdate \/ RecoverList \/ Crash \/ Finished

Spec == Init /\ [][Next]_vars
          /\ WF_vars(RecoverUpdate) /\ WF_vars(RecoverList)
          /\ \A j \in Jobs : WF_vars(Claim(j)) /\ WF_vars(Finish(j)) /\ WF_vars(Cleanup(j))
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
