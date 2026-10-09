----------------------- MODULE ReservedFinalizationWorkers -----------------------
EXTENDS Integers, FiniteSets
CONSTANTS Scopes, Hangable, MaxFailures
VARIABLES phase, worker, held, failures, inflight
vars == <<phase, worker, held, failures, inflight>>
Init ==
    /\ phase = [s \in Scopes |-> "new"] /\ worker = [s \in Scopes |-> "absent"]
    /\ held = {} /\ failures = [s \in Scopes |-> 0]
    /\ inflight = [s \in Scopes |-> 0]
Reserve(s) ==
    /\ phase[s] = "new" /\ phase' = [phase EXCEPT ![s] = "reserved"]
    /\ worker' = [worker EXCEPT ![s] = "idle"]
    /\ UNCHANGED <<held, failures, inflight>>
StartupFailure(s) ==
    /\ phase[s] = "new" /\ phase' = [phase EXCEPT ![s] = "aborted"]
    /\ UNCHANGED <<worker, held, failures, inflight>>
Acquire(s) ==
    /\ phase[s] = "reserved" /\ phase' = [phase EXCEPT ![s] = "active"]
    /\ held' = held \cup {s}
    /\ UNCHANGED <<worker, failures, inflight>>
Finalize(s) ==
    /\ phase[s] = "active" /\ phase' = [phase EXCEPT ![s] = "queued"]
    /\ UNCHANGED <<worker, held, failures, inflight>>
Start(s) ==
    /\ phase[s] = "queued" /\ worker[s] = "idle"
    /\ phase' = [phase EXCEPT ![s] = "running"]
    /\ worker' = [worker EXCEPT ![s] = "busy"] /\ inflight' = [inflight EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<held, failures>>
Fail(s) ==
    /\ phase[s] = "running" /\ failures[s] < MaxFailures
    /\ phase' = [phase EXCEPT ![s] = "queued"] /\ worker' = [worker EXCEPT ![s] = "idle"]
    /\ failures' = [failures EXCEPT ![s] = @ + 1] /\ inflight' = [inflight EXCEPT ![s] = @ - 1]
    /\ UNCHANGED <<held>>
Finish(s) ==
    /\ phase[s] = "running" /\ phase' = [phase EXCEPT ![s] = "done"]
    /\ worker' = [worker EXCEPT ![s] = "stopped"] /\ inflight' = [inflight EXCEPT ![s] = @ - 1]
    /\ held' = held \ {s} /\ UNCHANGED <<failures>>
Hang(s) ==
    /\ s \in Hangable /\ phase[s] = "running" /\ phase' = [phase EXCEPT ![s] = "hung"]
    /\ UNCHANGED <<worker, held, failures, inflight>>
Step(s) == Reserve(s) \/ StartupFailure(s) \/ Acquire(s) \/ Finalize(s)
    \/ Start(s) \/ Fail(s) \/ Finish(s) \/ Hang(s)
Next == (\E s \in Scopes : Step(s)) \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars /\ (\A s \in Scopes : WF_vars(Step(s)))
TypeOK == /\ phase \in [Scopes -> {"new", "reserved", "active", "queued", "running", "done", "aborted", "hung"}]
    /\ worker \in [Scopes -> {"absent", "idle", "busy", "stopped"}]
    /\ held \subseteq Scopes /\ failures \in [Scopes -> 0..MaxFailures]
    /\ inflight \in [Scopes -> 0..1]
ReservedBeforeAcquisition == \A s \in held : worker[s] \in {"idle", "busy"}
HeldUntilFinished == held = {s \in Scopes : phase[s] \in {"active", "queued", "running", "hung"}}
SingleAttempt == \A s \in Scopes : inflight[s] = IF phase[s] \in {"running", "hung"} THEN 1 ELSE 0
HealthyScopesFinish == \A s \in Scopes \ Hangable : <>(phase[s] \in {"done", "aborted"})
AllFinish == \A s \in Scopes : <>(phase[s] \in {"done", "aborted"})
=============================================================================
