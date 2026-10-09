---------------------------- MODULE MemcardPaths ----------------------------
EXTENDS Integers, FiniteSets
CONSTANTS Ids, Paths, PathOf, Versions, Policy, TornPath, DropOnMount, MaxEdits, MaxSessions
None == [id |-> "none", v |-> 0]
Files == {None} \cup [id : Ids, v : Versions]
Values == {0, -1} \cup Versions

VARIABLES phase, image, store, owned, known, resp, snapshot, synced, edits, sessions
vars == <<phase, image, store, owned, known, resp, snapshot, synced, edits, sessions>>

Present == {i \in Ids : image[i] # 0}
OwnClaim(i) == IF image[i] > 0 \/ TornPath THEN {PathOf[i]} ELSE {}
Claims(i) == OwnClaim(i) \cup known[i]
Claimed == UNION {Claims(i) : i \in Present}
Writable(i) ==
    /\ image[i] > 0
    /\ Policy = "lastwins" \/ \A j \in Present \ {i} : PathOf[i] \notin Claims(j)
Written == {i \in Present : Writable(i)}
WrittenPaths == {PathOf[i] : i \in Written}
Removed == IF Policy = "unguarded"
           THEN {p \in owned : \A i \in Present : p \notin known[i]}
           ELSE owned \ Claimed

Init ==
    /\ phase = "ended"
    /\ store \in {s \in [Paths -> Files] : \A p \in Paths : s[p] = None \/ PathOf[s[p].id] = p}
    /\ image = [i \in Ids |-> 0]
    /\ owned = {} /\ known = [i \in Ids |-> {}] /\ resp = {} /\ snapshot = store
    /\ synced = TRUE /\ edits = 0 /\ sessions = 0

Mount ==
    /\ phase = "ended" /\ sessions < MaxSessions
    /\ \E projected \in SUBSET {p \in Paths : store[p] # None} :
        /\ DropOnMount \/ projected = {p \in Paths : store[p] # None}
        /\ image' = [i \in Ids |-> IF PathOf[i] \in projected /\ store[PathOf[i]].id = i
                                   THEN store[PathOf[i]].v ELSE 0]
        /\ owned' = projected /\ resp' = projected
        /\ known' = [i \in Ids |-> IF PathOf[i] \in projected /\ store[PathOf[i]].id = i
                                   THEN {PathOf[i]} ELSE {}]
    /\ phase' = "running" /\ snapshot' = store /\ synced' = TRUE
    /\ sessions' = sessions + 1
    /\ UNCHANGED <<store, edits>>

Edit(i, value) ==
    /\ phase = "running" /\ edits < MaxEdits /\ image[i] # value
    /\ image' = [image EXCEPT ![i] = value]
    /\ synced' = FALSE /\ edits' = edits + 1
    /\ UNCHANGED <<phase, store, owned, known, resp, snapshot, sessions>>

Pass ==
    /\ phase = "running"
    /\ store' = [p \in Paths |->
                    IF p \in Removed THEN None
                    ELSE IF p \in WrittenPaths
                    THEN LET i == CHOOSE i \in Written : PathOf[i] = p IN [id |-> i, v |-> image[i]]
                    ELSE store[p]]
    /\ owned' = IF Policy = "fixed" THEN owned \ Removed ELSE (owned \ Removed) \cup WrittenPaths
    /\ known' = [i \in Ids |-> IF i \in Written THEN {PathOf[i]}
                               ELSE IF i \in Present THEN known[i] ELSE {}]
    /\ resp' = resp \cup WrittenPaths
    /\ synced' = TRUE
    /\ UNCHANGED <<phase, image, snapshot, edits, sessions>>

End ==
    /\ phase = "running"
    /\ phase' = "ended"
    /\ UNCHANGED <<image, store, owned, known, resp, snapshot, synced, edits, sessions>>

Next ==
    \/ Mount \/ Pass \/ End
    \/ \E i \in Ids, value \in Values : Edit(i, value)
    \/ UNCHANGED vars

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ store \in [Paths -> Files]
    /\ \A p \in Paths : store[p] = None \/ PathOf[store[p].id] = p
WrittenStored == synced => \A i \in Written : store[PathOf[i]] = [id |-> i, v |-> image[i]]
DeletedGone == synced => \A p \in resp : store[p] # None => p \in Claimed
NoCollateral == \A p \in Paths \ resp : store[p] = snapshot[p]
KeptUnchanged == [][Pass => \A i \in Present \ Written : \A p \in Claims(i) : store'[p] = store[p]]_vars
=============================================================================
