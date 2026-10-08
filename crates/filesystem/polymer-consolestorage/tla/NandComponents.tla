--------------------------- MODULE NandComponents ---------------------------
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Components, Revisions, Paths, SharedPaths, Ids, Absent,
          Owner, Artifact, Files, Overwrites, Target, IsUpdate,
          BaseIds, Depends, SfoPath, Mode, MaxSequence, Reorder

VARIABLES histories, tree, counter, uids, allocated
vars == <<histories, tree, counter, uids, allocated>>

EmptyTree == [p \in Paths |-> Absent]
EmptyHistory == [c \in Components |-> <<>>]
EntrySet(s) == {s[i] : i \in 1..Len(s)}
Min(S) == CHOOSE n \in S : \A m \in S : n <= m
Max(S) == CHOOSE n \in S : \A m \in S : n >= m

Selected(h) ==
    UNION {IF Mode = "ordered" THEN EntrySet(h[c])
           ELSE IF Len(h[c]) = 0 THEN {} ELSE {h[c][Len(h[c])]} : c \in Components}
ActiveIds(h) == {Artifact[e.rev] : e \in Selected(h)}

RECURSIVE Sorted(_)
Sorted(entries) ==
    IF entries = {} THEN <<>>
    ELSE LET e == CHOOSE e \in entries : e.seq = Min({x.seq : x \in entries})
         IN <<e>> \o Sorted(entries \ {e})

Ordered(h) ==
    IF Reorder /\ Mode = "ordered"
    THEN LET c == CHOOSE c \in Components : TRUE
         IN Sorted(EntrySet(h[c])) \o Sorted(UNION {EntrySet(h[d]) : d \in Components \ {c}})
    ELSE Sorted(Selected(h))

Apply(t, e) ==
    [p \in Paths |-> IF Files[e.rev][p] = Absent
                        \/ (Mode = "ordered" /\ p \notin Overwrites[e.rev] /\ t[p] # Absent)
                     THEN t[p] ELSE Files[e.rev][p]]

Accept(t, e) ==
    Mode # "ordered" \/
      /\ (Target[e.rev] = Absent \/ Target[e.rev] = t[SfoPath])
      /\ (Target[e.rev] # Absent \/ ~IsUpdate[e.rev] \/ t[SfoPath] = Absent
          \/ Files[e.rev][SfoPath] = Absent \/ Files[e.rev][SfoPath] >= t[SfoPath])

RECURSIVE Replay(_)
Replay(s) ==
    IF Len(s) = 0 THEN [files |-> EmptyTree, invalid |-> {}]
    ELSE LET prefix == Replay(SubSeq(s, 1, Len(s) - 1))
             e == s[Len(s)]
         IN [files |-> Apply(prefix.files, e),
             invalid |-> prefix.invalid \cup IF Accept(prefix.files, e) THEN {} ELSE {Artifact[e.rev]}]

Sources(entries, p) == {e \in entries : Files[e.rev][p] # Absent}
ReferenceFile(entries, p) ==
    LET writes == Sources(entries, p)
        replacing == {e \in writes : Mode # "ordered" \/ p \in Overwrites[e.rev]}
    IN IF writes = {} THEN Absent
       ELSE IF replacing = {}
       THEN Files[(CHOOSE e \in writes : e.seq = Min({w.seq : w \in writes})).rev][p]
       ELSE Files[(CHOOSE e \in replacing : e.seq = Max({w.seq : w \in replacing})).rev][p]

ReferenceTree(h) == [p \in Paths |-> ReferenceFile(Selected(h), p)]
ReferenceInvalid(h) ==
    {Artifact[e.rev] : e \in {e \in Selected(h) :
        ~Accept([p \in Paths |-> ReferenceFile({x \in Selected(h) : x.seq < e.seq}, p)], e)}}

Compatible(h) ==
    Mode = "ordered" \/
      \A p \in Paths :
        IF Mode = "shared" /\ p \in SharedPaths
        THEN Cardinality({Files[e.rev][p] : e \in Sources(Selected(h), p)}) <= 1
        ELSE Cardinality(Sources(Selected(h), p)) <= 1

Trim(s, ids) ==
    LET positions == {i \in 1..Len(s) : Artifact[s[i].rev] \in ids}
    IN IF positions = {} THEN s ELSE SubSeq(s, 1, Min(positions) - 1)
Retained(h, ids) ==
    [c \in Components |-> IF Mode = "ordered" THEN Trim(h[c], ids)
       ELSE IF Len(h[c]) = 0 THEN <<>>
       ELSE IF Artifact[h[c][Len(h[c])].rev] \in ids THEN <<>> ELSE h[c]]
PolicyAffected(h, ids) ==
    {Artifact[e.rev] : e \in {e \in Selected(h) : Depends[e.rev] \cap ids # {}}}
    \cup IF ids \cap BaseIds # {} THEN ActiveIds(h) ELSE {}

RECURSIVE Closure(_, _, _)
Closure(h, ids, n) ==
    IF n = 0 THEN ids
    ELSE LET kept == Retained(h, ids)
             more == ids \cup (ActiveIds(h) \ ActiveIds(kept))
                         \cup PolicyAffected(h, ids) \cup Replay(Ordered(kept)).invalid
         IN IF more = ids THEN ids ELSE Closure(h, more, n - 1)
Removing(h, ids) == Closure(h, ids, Cardinality(Ids))

InstallCandidate(c, r) == [histories EXCEPT ![c] = Append(@, [rev |-> r, seq |-> counter + 1])]
Valid(h) == Compatible(h) /\ Replay(Ordered(h)).invalid = {}
ActiveRevisions(h) == {e.rev : e \in Selected(h)}

Init ==
    /\ histories = EmptyHistory
    /\ tree = EmptyTree
    /\ counter = 0
    /\ uids = [c \in Components |-> 0]
    /\ allocated = 0

Install(r) ==
    LET c == Owner[r]
        candidate == InstallCandidate(c, r)
    IN /\ counter < MaxSequence
       /\ r \notin ActiveRevisions(histories)
       /\ Valid(candidate)
       /\ histories' = candidate
       /\ tree' = Replay(Ordered(candidate)).files
       /\ counter' = counter + 1
       /\ uids' = IF uids[c] = 0 THEN [uids EXCEPT ![c] = allocated + 1] ELSE uids
       /\ allocated' = IF uids[c] = 0 THEN allocated + 1 ELSE allocated

Restore(c, index) ==
    LET candidate == [histories EXCEPT ![c] = SubSeq(@, 1, index)]
    IN /\ index \in 1..(Len(histories[c]) - 1)
       /\ Valid(candidate)
       /\ histories' = candidate
       /\ tree' = Replay(Ordered(candidate)).files
       /\ UNCHANGED <<counter, uids, allocated>>

Uninstall(ids) ==
    LET removed == Removing(histories, ids)
        candidate == Retained(histories, ids)
    IN /\ removed = ids
       /\ ids \subseteq ActiveIds(histories)
       /\ ids # {}
       /\ Valid(candidate)
       /\ histories' = candidate
       /\ tree' = Replay(Ordered(candidate)).files
       /\ UNCHANGED <<counter, uids, allocated>>

Next == (\E r \in Revisions : Install(r))
        \/ (\E c \in Components, i \in 1..MaxSequence : Restore(c, i))
        \/ (\E ids \in SUBSET Ids : Uninstall(ids))

TypeOK ==
    /\ histories \in [Components -> Seq([rev : Revisions, seq : 1..MaxSequence])]
    /\ counter \in 0..MaxSequence
    /\ allocated \in 0..Cardinality(Components)
    /\ uids \in [Components -> 0..Cardinality(Components)]
    /\ \A c \in Components : \A e \in EntrySet(histories[c]) : Owner[e.rev] = c

HistoryOrdered ==
    \A c \in Components : \A i \in 1..(Len(histories[c]) - 1) :
        histories[c][i].seq < histories[c][i + 1].seq
SelectedValid == Valid(histories)
RemovalComplete ==
    \A ids \in SUBSET ActiveIds(histories) :
        ActiveIds(Retained(histories, Removing(histories, ids))) \cap ids = {}

UniqueSequence ==
    LET entries == UNION {EntrySet(histories[c]) : c \in Components}
    IN /\ Cardinality({e.seq : e \in entries}) = Cardinality(entries)
       /\ \A e \in entries : e.seq <= counter

CompositionMatchesReference == tree = ReferenceTree(histories)
VersionChecksMatchReference == Replay(Ordered(histories)).invalid = ReferenceInvalid(histories)
SharedContentSurvives ==
    Mode = "shared" => \A e \in Selected(histories) : \A p \in SharedPaths :
        Files[e.rev][p] # Absent => tree[p] = Files[e.rev][p]
AffectedComplete ==
    \A ids \in SUBSET ActiveIds(histories) :
      LET removed == Removing(histories, ids)
          candidate == Retained(histories, removed)
      IN /\ ReferenceInvalid(candidate) = {}
         /\ PolicyAffected(histories, removed) \subseteq removed
         /\ Removing(histories, removed) = removed
         /\ ActiveIds(histories) \ ActiveIds(candidate) \subseteq removed
RemovalOrderIndependent ==
    \A a, b \in SUBSET ActiveIds(histories) :
      ReferenceTree(Retained(Retained(histories, a), b)) = ReferenceTree(Retained(histories, a \cup b))
UIDsUnique == \A a, b \in Components : uids[a] # 0 /\ uids[a] = uids[b] => a = b
UIDsStable == [][\A c \in Components : uids[c] # 0 => uids'[c] = uids[c]]_vars

Spec == Init /\ [][Next]_vars
=============================================================================
