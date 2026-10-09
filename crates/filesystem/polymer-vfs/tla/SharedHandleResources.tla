---------------------- MODULE SharedHandleResources ----------------------
EXTENDS Integers, FiniteSets
CONSTANTS Launches, Design, MaxReads, MaxFailures
VARIABLES handle, launch, inst, state, refs, key, written, expect,
          created, reads, lastRead, failures, ext
vars == <<handle, launch, inst, state, refs, key, written, expect,
          created, reads, lastRead, failures, ext>>

Init ==
    /\ handle = "alive"
    /\ launch = [l \in Launches |-> "idle"]
    /\ inst = 0 /\ state = "none" /\ refs = {}
    /\ key = "free" /\ written = {} /\ expect = FALSE
    /\ created = 0 /\ reads = 0 /\ lastRead = "none"
    /\ failures = 0 /\ ext = "idle"

Materialize(holders) ==
    /\ inst' = created + 1 /\ created' = created + 1
    /\ state' = "live" /\ refs' = holders /\ key' = "res"

ReadResult(i) == IF expect /\ i \notin written THEN "lost" ELSE "ok"

Mount(l) ==
    /\ handle = "alive" /\ launch[l] = "idle"
    /\ CASE state = "live" /\ Design = "refcount" ->
                /\ refs' = refs \cup {l} /\ launch' = [launch EXCEPT ![l] = "active"]
                /\ UNCHANGED <<inst, state, key, created>>
         [] state = "none" /\ key = "free" ->
                /\ Materialize(IF Design = "refcount" THEN {"handle", l} ELSE {l})
                /\ launch' = [launch EXCEPT ![l] = "active"]
         [] OTHER ->
                /\ launch' = [launch EXCEPT ![l] = "failed"]
                /\ UNCHANGED <<inst, state, refs, key, created>>
    /\ UNCHANGED <<handle, written, expect, reads, lastRead, failures, ext>>

Write(l) ==
    /\ launch[l] = "active" /\ inst \notin written
    /\ written' = written \cup {inst}
    /\ expect' = (handle = "alive")
    /\ UNCHANGED <<handle, launch, inst, state, refs, key, created, reads, lastRead, failures, ext>>

Finish(l) ==
    /\ launch[l] = "active"
    /\ launch' = [launch EXCEPT ![l] = "done"]
    /\ refs' = refs \ {l}
    /\ UNCHANGED <<handle, inst, state, key, written, expect, created, reads, lastRead, failures, ext>>

ReadRefcount ==
    /\ CASE state = "live" ->
                /\ lastRead' = ReadResult(inst)
                /\ UNCHANGED <<inst, state, refs, key, created>>
         [] state = "none" /\ key = "free" ->
                /\ Materialize({"handle"})
                /\ lastRead' = ReadResult(created + 1)
         [] OTHER ->
                /\ lastRead' = "conflict"
                /\ UNCHANGED <<inst, state, refs, key, created>>

ReadWeak ==
    /\ CASE state = "live" ->
                /\ lastRead' = "shared"
                /\ UNCHANGED created
         [] state = "none" /\ key = "free" ->
                /\ created' = created + 1
                /\ lastRead' = ReadResult(created + 1)
         [] OTHER ->
                /\ lastRead' = "conflict"
                /\ UNCHANGED created
    /\ UNCHANGED <<inst, state, refs, key>>

Read ==
    /\ handle = "alive" /\ reads < MaxReads
    /\ reads' = reads + 1
    /\ IF Design = "refcount" THEN ReadRefcount ELSE ReadWeak
    /\ UNCHANGED <<handle, launch, written, expect, failures, ext>>

DropHandle ==
    /\ handle = "alive"
    /\ handle' = "dropped" /\ refs' = refs \ {"handle"} /\ expect' = FALSE
    /\ UNCHANGED <<launch, inst, state, key, written, created, reads, lastRead, failures, ext>>

BeginCleanup ==
    /\ state = "live" /\ refs = {}
    /\ state' = "cleaning"
    /\ UNCHANGED <<handle, launch, inst, refs, key, written, expect, created, reads, lastRead, failures, ext>>

CleanupFailure ==
    /\ state = "cleaning" /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ UNCHANGED <<handle, launch, inst, state, refs, key, written, expect, created, reads, lastRead, ext>>

Cleanup ==
    /\ state = "cleaning"
    /\ state' = "none" /\ inst' = 0 /\ key' = "free"
    /\ UNCHANGED <<handle, launch, refs, written, expect, created, reads, lastRead, failures, ext>>

ExtAcquire ==
    /\ ext = "idle" /\ key = "free"
    /\ ext' = "holding" /\ key' = "ext"
    /\ UNCHANGED <<handle, launch, inst, state, refs, written, expect, created, reads, lastRead, failures>>

ExtRelease ==
    /\ ext = "holding"
    /\ ext' = "done" /\ key' = "free"
    /\ UNCHANGED <<handle, launch, inst, state, refs, written, expect, created, reads, lastRead, failures>>

Done == handle = "dropped" /\ state = "none" /\ ext # "holding"
    /\ \A l \in Launches : launch[l] \in {"idle", "done", "failed"}

Next == (\E l \in Launches : Mount(l) \/ Write(l) \/ Finish(l))
    \/ Read \/ DropHandle \/ BeginCleanup \/ CleanupFailure \/ Cleanup
    \/ ExtAcquire \/ ExtRelease \/ (Done /\ UNCHANGED vars)

Spec == Init /\ [][Next]_vars
    /\ \A l \in Launches : WF_vars(Mount(l)) /\ WF_vars(Finish(l))
    /\ WF_vars(DropHandle) /\ WF_vars(BeginCleanup) /\ SF_vars(Cleanup)
    /\ WF_vars(ExtRelease)

TypeOK ==
    /\ handle \in {"alive", "dropped"}
    /\ launch \in [Launches -> {"idle", "active", "done", "failed"}]
    /\ state \in {"none", "live", "cleaning"}
    /\ refs \subseteq Launches \cup {"handle"}
    /\ key \in {"free", "res", "ext"}
    /\ lastRead \in {"none", "ok", "lost", "shared", "conflict"}
    /\ failures \in 0..MaxFailures
    /\ ext \in {"idle", "holding", "done"}
KeyProtected == state \in {"live", "cleaning"} => key = "res"
ReferencedIsLive == refs # {} => state = "live"
ActiveLaunchHeld == \A l \in Launches : launch[l] = "active" => l \in refs
HandleHeld == handle = "alive" /\ state # "none" => "handle" \in refs
CleanupUnreferenced == state = "cleaning" => refs = {}
OneInstancePerHandle == handle = "alive" => created <= 1
ReadNotRejectedByOwnMount == lastRead # "shared"
ReadSeesWrites == lastRead # "lost"
AllReleased == <>[](key # "res")
AllDone == <>Done
=============================================================================
