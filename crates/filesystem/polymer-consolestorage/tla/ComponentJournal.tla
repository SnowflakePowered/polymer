-------------------------- MODULE ComponentJournal --------------------------
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Volumes, Components, Paths, None, Values,
          MaxOperations, MaxCrashes, MaxFailures, MaxWrites, UseLease,
          ClearEarly, ReplacePending

VARIABLES refs, aggregate, checkout, index, journal, owner, phase, proposal,
          operations, crashes, failures, guests, acked, writes
vars == <<refs, aggregate, checkout, index, journal, owner, phase, proposal,
          operations, crashes, failures, guests, acked, writes>>

Heads == [Components -> Values]
EmptyHeads == [c \in Components |-> 0]
Image(h) == [p \in Paths |-> IF p \in Components THEN h[p]
                            ELSE IF \E c \in Components : h[c] # 0 THEN 1 ELSE 0]
Snapshot(h) == [heads |-> h, image |-> Image(h)]

Init ==
    /\ refs = [v \in Volumes |-> EmptyHeads]
    /\ aggregate = [v \in Volumes |-> Snapshot(EmptyHeads)]
    /\ checkout = [v \in Volumes |-> Image(EmptyHeads)]
    /\ index = checkout
    /\ journal = [v \in Volumes |-> None]
    /\ owner = None
    /\ phase = "idle"
    /\ proposal = None
    /\ operations = 0
    /\ crashes = 0
    /\ failures = 0
    /\ guests = {}
    /\ acked = [v \in Volumes |-> [p \in Paths |-> None]]
    /\ writes = 0

Start(v, h) ==
    /\ owner = None
    /\ operations < MaxOperations
    /\ journal[v] = None \/ ReplacePending
    /\ ~UseLease \/ v \notin guests
    /\ h # aggregate[v].heads
    /\ owner' = v
    /\ phase' = "stage"
    /\ proposal' = Snapshot(h)
    /\ operations' = operations + 1
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, crashes, failures, guests, acked, writes>>

Prepare ==
    /\ phase = "stage"
    /\ checkout[owner] = aggregate[owner].image
    /\ index[owner] = aggregate[owner].image
    /\ journal[owner] = None \/ ReplacePending
    /\ journal' = [journal EXCEPT ![owner] = [before |-> aggregate[owner], after |-> proposal]]
    /\ phase' = "refs"
    /\ UNCHANGED <<refs, aggregate, checkout, index, owner, proposal, operations, crashes, failures, guests, acked, writes>>

RejectStage ==
    /\ phase = "stage"
    /\ owner' = None
    /\ phase' = "idle"
    /\ proposal' = None
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, operations, crashes, failures, guests, acked, writes>>

PublishComponent(c) ==
    /\ phase = "refs"
    /\ refs[owner][c] # journal[owner].after.heads[c]
    /\ refs[owner][c] = journal[owner].before.heads[c]
    /\ refs' = [refs EXCEPT ![owner][c] = journal[owner].after.heads[c]]
    /\ UNCHANGED <<aggregate, checkout, index, journal, owner, phase, proposal, operations, crashes, failures, guests, acked, writes>>

PublishAggregate ==
    /\ phase = "refs"
    /\ refs[owner] = journal[owner].after.heads
    /\ aggregate[owner] \in {journal[owner].before, journal[owner].after}
    /\ aggregate' = [aggregate EXCEPT ![owner] = journal[owner].after]
    /\ phase' = "checkout"
    /\ UNCHANGED <<refs, checkout, index, journal, owner, proposal, operations, crashes, failures, guests, acked, writes>>

WriteFile(p) ==
    /\ phase = "checkout"
    /\ checkout[owner][p] # journal[owner].after.image[p]
    /\ checkout' = [checkout EXCEPT ![owner][p] = journal[owner].after.image[p]]
    /\ UNCHANGED <<refs, aggregate, index, journal, owner, phase, proposal, operations, crashes, failures, guests, acked, writes>>

WriteIndex ==
    /\ phase = "checkout"
    /\ checkout[owner] = journal[owner].after.image
    /\ index' = [index EXCEPT ![owner] = journal[owner].after.image]
    /\ phase' = "clear"
    /\ UNCHANGED <<refs, aggregate, checkout, journal, owner, proposal, operations, crashes, failures, guests, acked, writes>>

Finish ==
    /\ phase = "clear" \/ (ClearEarly /\ phase = "checkout")
    /\ journal' = [journal EXCEPT ![owner] = None]
    /\ owner' = None
    /\ phase' = "idle"
    /\ proposal' = None
    /\ UNCHANGED <<refs, aggregate, checkout, index, operations, crashes, failures, guests, acked, writes>>

Recover(v) ==
    /\ owner = None
    /\ journal[v] # None
    /\ ~UseLease \/ v \notin guests
    /\ owner' = v
    /\ phase' = "refs"
    /\ proposal' = None
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, operations, crashes, failures, guests, acked, writes>>

Fail ==
    /\ owner # None
    /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ owner' = None
    /\ phase' = "idle"
    /\ proposal' = None
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, operations, crashes, guests, acked, writes>>

Crash ==
    /\ crashes < MaxCrashes
    /\ crashes' = crashes + 1
    /\ owner' = None
    /\ phase' = "idle"
    /\ proposal' = None
    /\ guests' = {}
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, operations, failures, acked, writes>>

Mount(v) ==
    /\ v \notin guests
    /\ ~UseLease \/ (owner # v /\ journal[v] = None)
    /\ guests' = guests \cup {v}
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, owner, phase, proposal, operations, crashes, failures, acked, writes>>

Unmount(v) ==
    /\ v \in guests
    /\ guests' = guests \ {v}
    /\ UNCHANGED <<refs, aggregate, checkout, index, journal, owner, phase, proposal, operations, crashes, failures, acked, writes>>

GuestWrite(v, p, value) ==
    /\ v \in guests
    /\ writes < MaxWrites
    /\ checkout' = [checkout EXCEPT ![v][p] = value]
    /\ acked' = [acked EXCEPT ![v][p] = IF value = aggregate[v].image[p] THEN None ELSE value]
    /\ writes' = writes + 1
    /\ UNCHANGED <<refs, aggregate, index, journal, owner, phase, proposal, operations, crashes, failures, guests>>

DiscardGuest(v) ==
    /\ owner = None
    /\ v \notin guests
    /\ journal[v] = None
    /\ checkout' = [checkout EXCEPT ![v] = aggregate[v].image]
    /\ index' = [index EXCEPT ![v] = aggregate[v].image]
    /\ acked' = [acked EXCEPT ![v] = [p \in Paths |-> None]]
    /\ UNCHANGED <<refs, aggregate, journal, owner, phase, proposal, operations, crashes, failures, guests, writes>>

Progress == Prepare \/ RejectStage \/ (\E c \in Components : PublishComponent(c))
            \/ PublishAggregate \/ (\E p \in Paths : WriteFile(p)) \/ WriteIndex \/ Finish
            \/ (\E v \in Volumes : Recover(v))
Next == Progress \/ Fail \/ Crash
        \/ (\E v \in Volumes, h \in Heads : Start(v, h))
        \/ (\E v \in Volumes : Mount(v) \/ Unmount(v) \/ DiscardGuest(v))
        \/ (\E v \in Volumes, p \in Paths, value \in Values : GuestWrite(v, p, value))

TypeOK ==
    /\ refs \in [Volumes -> Heads]
    /\ aggregate \in [Volumes -> {Snapshot(h) : h \in Heads}]
    /\ checkout \in [Volumes -> [Paths -> Values]]
    /\ index \in [Volumes -> [Paths -> Values]]
    /\ journal \in [Volumes -> {None} \cup [before : {Snapshot(h) : h \in Heads}, after : {Snapshot(h) : h \in Heads}]]
    /\ proposal \in {None} \cup {Snapshot(h) : h \in Heads}
    /\ acked \in [Volumes -> [Paths -> Values \cup {None}]]
    /\ operations \in 0..MaxOperations /\ crashes \in 0..MaxCrashes
    /\ failures \in 0..MaxFailures /\ writes \in 0..MaxWrites
    /\ guests \subseteq Volumes
    /\ owner \in Volumes \cup {None}
    /\ phase \in {"idle", "stage", "refs", "checkout", "clear"}
    /\ (owner = None) = (phase = "idle")
    /\ phase \in {"refs", "checkout", "clear"} => journal[owner] # None

PendingReferences ==
    \A v \in Volumes : journal[v] # None =>
      /\ aggregate[v] \in {journal[v].before, journal[v].after}
      /\ \A c \in Components : refs[v][c] \in {journal[v].before.heads[c], journal[v].after.heads[c]}
LeaseExcludesGuests == UseLease =>
    \A v \in guests : owner # v /\ journal[v] = None
RecoveryEnabled == (\E v \in Volumes : journal[v] # None) => ENABLED Progress

SettledReferences == \A v \in Volumes : journal[v] = None => refs[v] = aggregate[v].heads
SettledIndex == \A v \in Volumes : journal[v] = None => index[v] = aggregate[v].image
CheckoutMatchesOrDirty ==
    \A v \in Volumes : journal[v] = None =>
      \A p \in Paths : checkout[v][p] = aggregate[v].image[p] \/ acked[v][p] = checkout[v][p]
NoAcknowledgedWriteErased ==
    \A v \in Volumes : \A p \in Paths : acked[v][p] # None => checkout[v][p] = acked[v][p]
JournalNotReplaced ==
    [][\A v \in Volumes : journal[v] # None /\ journal'[v] # None => journal[v] = journal'[v]]_vars
RecoveryCompletes == \A v \in Volumes : journal[v] # None ~> journal[v] = None

Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
=============================================================================
