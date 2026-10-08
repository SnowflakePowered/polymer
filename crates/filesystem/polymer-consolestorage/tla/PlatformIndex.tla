---------------------------- MODULE PlatformIndex ----------------------------
EXTENDS TLC

CONSTANTS Volumes, SharedLock, None
VARIABLES index, snapshots, pc, owner
vars == <<index, snapshots, pc, owner>>

Init ==
    /\ index = [v \in Volumes |-> 0]
    /\ snapshots = [v \in Volumes |-> index]
    /\ pc = [v \in Volumes |-> "idle"]
    /\ owner = None

ReadIndex(v) ==
    /\ pc[v] = "idle"
    /\ ~SharedLock \/ owner = None
    /\ owner' = IF SharedLock THEN v ELSE owner
    /\ snapshots' = [snapshots EXCEPT ![v] = index]
    /\ pc' = [pc EXCEPT ![v] = "write"]
    /\ UNCHANGED index

WriteIndex(v) ==
    /\ pc[v] = "write"
    /\ index' = [snapshots[v] EXCEPT ![v] = 1]
    /\ pc' = [pc EXCEPT ![v] = "done"]
    /\ owner' = None
    /\ UNCHANGED snapshots

Next == \E v \in Volumes : ReadIndex(v) \/ WriteIndex(v)
CommittedEntriesPreserved == \A v \in Volumes : pc[v] = "done" => index[v] = 1
AllPublish == <> (\A v \in Volumes : pc[v] = "done")
Spec == Init /\ [][Next]_vars
        /\ (\A v \in Volumes : WF_vars(ReadIndex(v)) /\ WF_vars(WriteIndex(v)))
=============================================================================
