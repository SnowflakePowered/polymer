---------------------- MODULE ManagedResourceLeases ----------------------
EXTENDS Integers, Sequences, FiniteSets, TLC
CONSTANTS Clients, Jobs, Keys, Required, Free, MaxFailures, EarlyRelease
VARIABLES holder, phase, acquired, registered, cursor, successes, watcher,
          failures, job
vars == <<holder, phase, acquired, registered, cursor, successes, watcher,
          failures, job>>

Init ==
    /\ holder = [k \in Keys |-> Free]
    /\ phase = [c \in Clients |-> "acquire"]
    /\ acquired = [c \in Clients |-> 0]
    /\ registered = [c \in Clients |-> 0]
    /\ cursor = [c \in Clients |-> 1]
    /\ successes = [c \in Clients |-> [i \in 1..2 |-> 0]]
    /\ watcher = [c \in Clients |-> FALSE]
    /\ failures = 0
    /\ job = [j \in Jobs |-> "waiting"]

Acquire(c) ==
    /\ phase[c] = "acquire" /\ acquired[c] < Len(Required[c])
    /\ LET k == Required[c][acquired[c] + 1] IN
        /\ holder[k] = Free
        /\ holder' = [holder EXCEPT ![k] = c]
    /\ acquired' = [acquired EXCEPT ![c] = @ + 1]
    /\ UNCHANGED <<phase, registered, cursor, successes, watcher, failures, job>>

Build(c) ==
    /\ phase[c] = "acquire" /\ acquired[c] = Len(Required[c])
    /\ phase' = [phase EXCEPT ![c] = "build"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, failures, job>>

Register(c) ==
    /\ phase[c] = "build" /\ registered[c] < 2
    /\ registered' = [registered EXCEPT ![c] = @ + 1]
    /\ watcher' = [watcher EXCEPT ![c] = (registered'[c] = 2)]
    /\ UNCHANGED <<holder, phase, acquired, cursor, successes, failures, job>>

Ready(c) ==
    /\ phase[c] = "build" /\ registered[c] = 2
    /\ phase' = [phase EXCEPT ![c] = "ready"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, failures, job>>

Finalize(c) ==
    /\ phase[c] \in {"acquire", "build", "ready", "retained"}
    /\ phase' = [phase EXCEPT ![c] = "finalize"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, failures, job>>

Cleanup(c) ==
    /\ phase[c] = "finalize" /\ cursor[c] <= registered[c]
    /\ successes' = [successes EXCEPT ![c][cursor[c]] = @ + 1]
    /\ watcher' = [watcher EXCEPT ![c] = IF cursor[c] = 2 THEN FALSE ELSE @]
    /\ cursor' = [cursor EXCEPT ![c] = @ + 1]
    /\ holder' = IF EarlyRelease /\ cursor[c] = 1
                 THEN [k \in Keys |-> IF holder[k] = c THEN Free ELSE holder[k]] ELSE holder
    /\ UNCHANGED <<phase, acquired, registered, failures, job>>

CleanupFailure(c) ==
    /\ phase[c] = "finalize" /\ cursor[c] <= registered[c]
    /\ failures < MaxFailures /\ failures' = failures + 1
    /\ phase' = [phase EXCEPT ![c] = "retained"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, job>>

Abandon(c) ==
    /\ phase[c] = "finalize" /\ cursor[c] <= registered[c]
    /\ phase' = [phase EXCEPT ![c] = "retained"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, failures, job>>

Retry(c) ==
    /\ phase[c] = "retained"
    /\ phase' = [phase EXCEPT ![c] = "finalize"]
    /\ UNCHANGED <<holder, acquired, registered, cursor, successes, watcher, failures, job>>

Release(c) ==
    /\ phase[c] = "finalize" /\ cursor[c] > registered[c]
    /\ holder' = [k \in Keys |-> IF holder[k] = c THEN Free ELSE holder[k]]
    /\ phase' = [phase EXCEPT ![c] = "done"]
    /\ UNCHANGED <<acquired, registered, cursor, successes, watcher, failures, job>>

JobAcquire(j) ==
    /\ job[j] = "waiting"
    /\ \E k \in Keys :
        /\ holder[k] = Free
        /\ holder' = [holder EXCEPT ![k] = j]
    /\ job' = [job EXCEPT ![j] = "work"]
    /\ UNCHANGED <<phase, acquired, registered, cursor, successes, watcher, failures>>

JobRelease(j) ==
    /\ job[j] = "work"
    /\ holder' = [k \in Keys |-> IF holder[k] = j THEN Free ELSE holder[k]]
    /\ job' = [job EXCEPT ![j] = "done"]
    /\ UNCHANGED <<phase, acquired, registered, cursor, successes, watcher, failures>>

ClientStep(c) == Acquire(c) \/ Build(c) \/ Register(c) \/ Ready(c) \/ Finalize(c)
    \/ Cleanup(c) \/ CleanupFailure(c) \/ Abandon(c) \/ Retry(c) \/ Release(c)
Next == (\E c \in Clients : ClientStep(c)) \/ (\E j \in Jobs : JobAcquire(j) \/ JobRelease(j))
    \/ ((\A c \in Clients : phase[c] = "done") /\ (\A j \in Jobs : job[j] = "done") /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars
    /\ \A c \in Clients : SF_vars(Cleanup(c)) /\ WF_vars(Release(c))
        /\ WF_vars(Retry(c)) /\ WF_vars(Finalize(c))
    /\ \A j \in Jobs : WF_vars(JobAcquire(j)) /\ WF_vars(JobRelease(j))

TypeOK ==
    /\ holder \in [Keys -> Clients \cup Jobs \cup {Free}]
    /\ phase \in [Clients -> {"acquire", "build", "ready", "finalize", "retained", "done"}]
    /\ registered \in [Clients -> 0..2] /\ cursor \in [Clients -> 1..3]
    /\ watcher \in [Clients -> BOOLEAN] /\ failures \in 0..MaxFailures
    /\ \A c \in Clients : acquired[c] \in 0..Len(Required[c])
    /\ successes \in [Clients -> [1..2 -> Nat]]
    /\ job \in [Jobs -> {"waiting", "work", "done"}]
WatcherProtected == \A c \in Clients : watcher[c] =>
    \A i \in 1..Len(Required[c]) : holder[Required[c][i]] = c
HeldThroughCleanup == \A c \in Clients : phase[c] # "done" =>
    \A i \in 1..acquired[c] : holder[Required[c][i]] = c
CompletedOnce == \A c \in Clients : \A i \in 1..2 :
    successes[c][i] = IF i < cursor[c] THEN 1 ELSE 0
OrderedCleanup == \A c \in Clients : successes[c][2] = 1 => successes[c][1] = 1
ReleasedOnExit == \A c \in Clients : phase[c] = "done" =>
    ~watcher[c] /\ cursor[c] > registered[c] /\ (\A k \in Keys : holder[k] # c)
AllReleased == <>[](\A k \in Keys : holder[k] = Free)
AllTerminate == <>(\A c \in Clients : phase[c] = "done")
=============================================================================
