----------------------- MODULE SharedResourceLeases -----------------------
EXTENDS Integers, FiniteSets, TLC
CONSTANTS Parents, JointRoot, Alias, MaxFailures
VARIABLES phase, watcher, dependencyClean, dependencyReleased, completions,
          holder, aliasDone, failures, job
vars == <<phase, watcher, dependencyClean, dependencyReleased, completions,
          holder, aliasDone, failures, job>>

NeedsDependency(p) == phase[p] \in {"ready", "dependency", "tail"}
Init ==
    /\ phase = [p \in Parents |-> "ready"]
    /\ watcher = [p \in Parents |-> TRUE]
    /\ dependencyClean = FALSE /\ dependencyReleased = FALSE /\ completions = 0
    /\ holder = "dependency" /\ aliasDone = FALSE /\ failures = 0 /\ job = "waiting"

Begin(p) ==
    /\ phase[p] = "ready"
    /\ phase' = [phase EXCEPT ![p] = "dependency"]
    /\ UNCHANGED <<watcher, dependencyClean, dependencyReleased, completions, holder, aliasDone, failures, job>>

CleanDependency(p) ==
    /\ phase[p] = "dependency"
    /\ phase' = [phase EXCEPT ![p] = "tail"]
    /\ dependencyClean' = TRUE
    /\ completions' = IF dependencyClean THEN completions ELSE completions + 1
    /\ UNCHANGED <<watcher, dependencyReleased, holder, aliasDone, failures, job>>

CleanupFailure(p) ==
    /\ phase[p] \in {"dependency", "tail"} /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ UNCHANGED <<phase, watcher, dependencyClean, dependencyReleased, completions, holder, aliasDone, job>>

StopWatcher(p) ==
    /\ phase[p] = "tail"
    /\ phase' = [phase EXCEPT ![p] = "clean"]
    /\ watcher' = [watcher EXCEPT ![p] = FALSE]
    /\ UNCHANGED <<dependencyClean, dependencyReleased, completions, holder, aliasDone, failures, job>>

Release(p) ==
    /\ phase[p] = "clean"
    /\ ~JointRoot \/ (\A q \in Parents : ~NeedsDependency(q))
    /\ phase' = [phase EXCEPT ![p] = "released"]
    /\ dependencyReleased' = TRUE
    /\ holder' = IF dependencyReleased THEN holder ELSE "free"
    /\ UNCHANGED <<watcher, dependencyClean, completions, aliasDone, failures, job>>

FinalizeAlias ==
    /\ Alias /\ ~aliasDone
    /\ aliasDone' = TRUE /\ dependencyClean' = TRUE /\ dependencyReleased' = TRUE
    /\ completions' = IF dependencyClean THEN completions ELSE completions + 1
    /\ holder' = IF dependencyReleased THEN holder ELSE "free"
    /\ UNCHANGED <<phase, watcher, failures, job>>

JobAcquire ==
    /\ job = "waiting" /\ holder = "free"
    /\ job' = "working" /\ holder' = "job"
    /\ UNCHANGED <<phase, watcher, dependencyClean, dependencyReleased, completions, aliasDone, failures>>

JobRelease ==
    /\ job = "working"
    /\ job' = "done" /\ holder' = "free"
    /\ UNCHANGED <<phase, watcher, dependencyClean, dependencyReleased, completions, aliasDone, failures>>

ParentStep(p) == Begin(p) \/ CleanDependency(p) \/ CleanupFailure(p) \/ StopWatcher(p) \/ Release(p)
Next == (\E p \in Parents : ParentStep(p)) \/ FinalizeAlias \/ JobAcquire \/ JobRelease \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars /\ (\A p \in Parents : WF_vars(ParentStep(p)))
    /\ WF_vars(FinalizeAlias) /\ WF_vars(JobAcquire) /\ WF_vars(JobRelease)

TypeOK ==
    /\ phase \in [Parents -> {"ready", "dependency", "tail", "clean", "released"}]
    /\ watcher \in [Parents -> BOOLEAN]
    /\ dependencyClean \in BOOLEAN /\ dependencyReleased \in BOOLEAN /\ aliasDone \in BOOLEAN
    /\ completions \in Nat /\ failures \in 0..MaxFailures
    /\ holder \in {"dependency", "free", "job"} /\ job \in {"waiting", "working", "done"}
DependencyProtected == (\E p \in Parents : NeedsDependency(p)) => holder = "dependency"
WatcherProtected == (\E p \in Parents : watcher[p]) => holder = "dependency"
NoConcurrentJob == job = "working" => \A p \in Parents : ~NeedsDependency(p)
CleanupOnce == completions <= 1
ReleaseAfterOwnCleanup == dependencyReleased => dependencyClean
AllReleased == <>((\A p \in Parents : phase[p] = "released") /\ job = "done" /\ holder = "free")
=============================================================================
