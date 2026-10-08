--------------------------- MODULE ComposedRename ---------------------------
EXTENDS Integers, Sequences, FiniteSets, TLC
CONSTANTS Layers, Durable, Paths, Owner, FilePaths, RenameOps, InitNs, ResetNs,
          Handles, Writable, OpenPaths, CreatePaths, ReplacePaths,
          MaxObjs, MaxWrites, MaxRenames, MaxCrashes, MaxFailures,
          LockIO, StableHandles, Journaled, RemoveFirst
DIR == -1
NoPath == <<>>
NoB == "nob"
Objs == 1..MaxObjs
Writes == 1..MaxWrites
NoOp == [from |-> NoPath, to |-> NoPath, cross |-> FALSE]

VARIABLES identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, rpc, rop, txn, staged, committed, published, denied, prior
vars == <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, rpc, rop, txn, staged, committed, published, denied, prior>>


Parent(p) == SubSeq(p, 1, Len(p) - 1)
Staged(to) == Parent(to) \o <<"~" \o to[Len(to)]>>
StagedPaths == {Staged(p) : p \in FilePaths} \cap Paths
Under(p, root) == Len(p) >= Len(root) /\ SubSeq(p, 1, Len(root)) = root
Rebase(p, from, to) == to \o SubSeq(p, Len(from) + 1, Len(p))
At(p) == ns[Owner[p]][p]
IsFile(p) == At(p) > 0
Idle == rpc = "idle"
Ordered == RemoveFirst /\ ~Journaled
Available == ~LockIO \/ Idle
Live(b) == b # NoB /\ \E h \in Handles : hst[h] = "open" /\ hb[h] = b
Redirected(h) == redir[hb[h]] # NoPath /\ redir[hb[h]] # hat[h]
Target(h) ==
    IF StableHandles THEN IF bound[hb[h]] = 0 THEN hnat[h] ELSE bound[hb[h]]
    ELSE IF ~Redirected(h) THEN hnat[h]
    ELSE IF IsFile(redir[hb[h]]) THEN At(redir[hb[h]]) ELSE 0
CanWrite(h) == h \in Writable \/ (~StableHandles /\ Redirected(h))
WithParent(m, p) == IF Len(p) > 1 THEN [m EXCEPT ![Parent(p)] = DIR] ELSE m
ParentOk(m, p) == IF Len(p) = 1 THEN TRUE ELSE m[Parent(p)] \in {0, DIR}
NativeRename(m, from, to, empty) ==
    [p \in Paths |-> IF Under(p, from) THEN empty
      ELSE IF \E q \in Paths : Under(q, from) /\ Rebase(q, from, to) = p
      THEN m[CHOOSE q \in Paths : Under(q, from) /\ Rebase(q, from, to) = p]
      ELSE IF Under(p, to) THEN empty ELSE m[p]]
AckedPath(p, from, to) ==
    IF p = NoPath THEN NoPath
    ELSE IF Under(p, to) THEN NoPath
    ELSE IF Under(p, from) THEN Rebase(p, from, to) ELSE p
ResetObjects == {o \in Objs : \E l \in Layers \ Durable, p \in Paths : ResetNs[l][p] = o}
CrashPath(h) ==
    IF Journaled /\ txn # NoOp /\ committed /\ ~published
    THEN AckedPath(cur[h], txn.from, txn.to) ELSE cur[h]
Init ==
    /\ identity = [h \in Handles |-> <<1, 0>>]
    /\ ns = InitNs
    /\ nextObj = 1 + Cardinality({o \in Objs : \E l \in Layers, p \in Paths : InitNs[l][p] = o})
    /\ data = [o \in Objs |-> {}]
    /\ lid = [o \in Objs |-> <<o, 0>>]
    /\ hst = [h \in Handles |-> "unused"]
    /\ hat = [h \in Handles |-> NoPath]
    /\ hnat = [h \in Handles |-> 0]
    /\ hb = [h \in Handles |-> NoB]
    /\ files = [p \in Paths |-> NoB]
    /\ redir = [h \in Handles |-> NoPath]
    /\ bound = [h \in Handles |-> 0]
    /\ cur = [h \in Handles |-> NoPath]
    /\ acked = [h \in Handles |-> {}]
    /\ nextW = 1 /\ renames = 0 /\ crashes = 0 /\ failures = 0
    /\ rpc = "idle" /\ rop = NoOp /\ txn = NoOp
    /\ staged = 0 /\ committed = FALSE /\ published = FALSE /\ denied = {}
    /\ prior = [source |-> 0, destination |-> 0, identity |-> <<0, 0>>]

Open(h, p) ==
    /\ hst[h] = "unused" /\ p \in OpenPaths /\ IsFile(p)
    /\ Available
    /\ Idle \/ ~(Under(p, rop.from) \/ Under(p, rop.to))
    /\ hst' = [hst EXCEPT ![h] = "open"]
    /\ hat' = [hat EXCEPT ![h] = p]
    /\ identity' = [identity EXCEPT ![h] = lid[At(p)]]
    /\ hnat' = [hnat EXCEPT ![h] = At(p)]
    /\ cur' = [cur EXCEPT ![h] = p]
    /\ IF Live(files[p])
       THEN /\ hb' = [hb EXCEPT ![h] = files[p]] /\ UNCHANGED <<files, redir, bound>>
       ELSE /\ hb' = [hb EXCEPT ![h] = h]
            /\ files' = [files EXCEPT ![p] = h]
            /\ redir' = [redir EXCEPT ![h] = NoPath]
            /\ bound' = [bound EXCEPT ![h] = 0]
    /\ UNCHANGED <<ns, data, lid, nextObj, acked, nextW, renames, crashes, failures, rpc, rop, txn, staged, committed, published, denied, prior>>

Create(p) ==
    /\ Available /\ nextObj <= MaxObjs /\ ParentOk(ns[Owner[p]], p)
    /\ (p \in CreatePaths /\ At(p) = 0) \/ (p \in ReplacePaths /\ IsFile(p))
    /\ ns' = [ns EXCEPT ![Owner[p]] = [WithParent(@, p) EXCEPT ![p] = nextObj]]
    /\ nextObj' = nextObj + 1
    /\ cur' = [h \in Handles |-> IF cur[h] = p THEN NoPath ELSE cur[h]]
    /\ files' = IF StableHandles THEN [files EXCEPT ![p] = NoB] ELSE files
    /\ UNCHANGED <<identity, data, lid, hst, hat, hnat, hb, redir, bound, acked, nextW, renames, crashes, failures, rpc, rop, txn, staged, committed, published, denied, prior>>

WriteAt(h) ==
    /\ Available /\ hst[h] = "open" /\ nextW <= MaxWrites /\ Target(h) > 0
    /\ CanWrite(h)
    /\ data' = [data EXCEPT ![Target(h)] = @ \cup {nextW}]
    /\ acked' = [acked EXCEPT ![h] = @ \cup {nextW}]
    /\ nextW' = nextW + 1
    /\ denied' = IF h \in Writable THEN denied ELSE denied \cup {h}
    /\ UNCHANGED <<identity, ns, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, renames, crashes, failures, rpc, rop, txn, staged, committed, published, prior>>

Rename(from, to) ==
    /\ Idle /\ renames < MaxRenames /\ <<from, to>> \in RenameOps
    /\ At(from) # 0 /\ ParentOk(ns[Owner[to]], to)
    /\ Owner[from] = Owner[to] \/ IsFile(from)
    /\ renames' = renames + 1
    /\ rop' = [from |-> from, to |-> to, cross |-> Owner[from] # Owner[to]]
    /\ prior' = [source |-> At(from), destination |-> At(to),
                   identity |-> IF IsFile(from) THEN lid[At(from)] ELSE <<0, 0>>]
    /\ committed' = FALSE /\ staged' = 0
    /\ IF Owner[from] = Owner[to]
       THEN /\ ns' = [ns EXCEPT ![Owner[from]] = NativeRename(@, from, to, 0)]
            /\ rpc' = "bind" /\ published' = TRUE /\ txn' = NoOp
            /\ cur' = [h \in Handles |-> AckedPath(cur[h], from, to)]
       ELSE /\ rpc' = "copy" /\ published' = FALSE
            /\ txn' = IF Journaled THEN rop' ELSE NoOp
            /\ UNCHANGED <<identity, ns, cur>>
    /\ UNCHANGED <<identity, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, acked, nextW, crashes, failures, denied>>

CopyToStaged ==
    /\ rpc = "copy"
    /\ IF IsFile(rop.from) /\ nextObj <= MaxObjs /\ ParentOk(ns[Owner[rop.to]], Staged(rop.to))
       THEN /\ ns' = IF Journaled THEN ns
                     ELSE [ns EXCEPT ![Owner[rop.to]] = [WithParent(@, Staged(rop.to)) EXCEPT ![Staged(rop.to)] = nextObj]]
            /\ data' = [data EXCEPT ![nextObj] = data[At(rop.from)]]
            /\ lid' = [lid EXCEPT ![nextObj] = lid[At(rop.from)]]
            /\ staged' = nextObj /\ nextObj' = nextObj + 1
            /\ rpc' = IF Journaled THEN "decide" ELSE IF Ordered THEN "remove" ELSE "publish"
       ELSE /\ rpc' = "abort"
            /\ UNCHANGED <<identity, ns, data, lid, staged, nextObj>>
    /\ UNCHANGED <<identity, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, rop, txn, committed, published, denied, prior>>

CommitDecision ==
    /\ rpc = "decide" /\ staged > 0 /\ txn # NoOp
    /\ committed' = TRUE /\ rpc' = "publish"
    /\ UNCHANGED <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, rop, txn, staged, published, denied, prior>>

PublishStaged ==
    /\ rpc = "publish"
    /\ staged > 0
    /\ ns' = [ns EXCEPT ![Owner[rop.to]] =
         [WithParent(@, rop.to) EXCEPT ![rop.to] = staged,
                                      ![Staged(rop.to)] = 0]]
    /\ cur' = IF published THEN cur
              ELSE [h \in Handles |-> AckedPath(cur[h], rop.from, rop.to)]
    /\ published' = TRUE /\ rpc' = IF Ordered THEN "bind" ELSE "remove"
    /\ UNCHANGED <<identity, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, acked, nextW, renames, crashes, failures, rop, txn, staged, committed, denied, prior>>

RemoveFrom ==
    /\ rpc = "remove"
    /\ ns' = IF Journaled /\ (~IsFile(rop.from) \/ lid[At(rop.from)] # prior.identity)
             THEN ns ELSE [ns EXCEPT ![Owner[rop.from]][rop.from] = 0]
    /\ rpc' = IF Ordered THEN "publish" ELSE "bind"
    /\ UNCHANGED <<identity, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, rop, txn, staged, committed, published, denied, prior>>

RemoveFailure ==
    /\ rpc = "remove" /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ rpc' = IF Journaled THEN "remove" ELSE IF Ordered THEN "abort" ELSE "bind"
    /\ UNCHANGED <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, rop, txn, staged, committed, published, denied, prior>>

MoveBinding ==
    /\ rpc = "bind"
    /\ LET src == IF Live(files[rop.from]) THEN files[rop.from] ELSE NoB IN
       /\ files' = IF StableHandles
                    THEN NativeRename(files, rop.from, rop.to, NoB)
                    ELSE [files EXCEPT ![rop.from] = NoB, ![rop.to] = src]
       /\ redir' = IF ~StableHandles /\ src # NoB /\ (rop.cross \/ redir[src] # NoPath)
                    THEN [redir EXCEPT ![src] = rop.to] ELSE redir
       /\ bound' = IF StableHandles /\ src # NoB /\ rop.cross
                    THEN [bound EXCEPT ![src] = At(rop.to)] ELSE bound
    /\ rpc' = IF txn # NoOp THEN "clear" ELSE "idle"
    /\ rop' = IF txn # NoOp THEN rop ELSE NoOp
    /\ UNCHANGED <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, cur, acked, nextW, renames, crashes, failures, txn, staged, committed, published, denied, prior>>

Clear ==
    /\ rpc = "clear"
    /\ rpc' = "idle" /\ rop' = NoOp /\ txn' = NoOp
    /\ staged' = 0 /\ committed' = FALSE
    /\ UNCHANGED <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, published, denied, prior>>

Abort ==
    /\ rpc = "abort"
    /\ ns' = IF Journaled \/ staged = 0 THEN ns
             ELSE [ns EXCEPT ![Owner[rop.to]][Staged(rop.to)] = 0]
    /\ rpc' = "idle" /\ rop' = NoOp /\ txn' = NoOp
    /\ staged' = 0 /\ committed' = FALSE
    /\ UNCHANGED <<identity, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, failures, published, denied, prior>>

Restore ==
    /\ rpc = "restore" /\ nextObj <= MaxObjs
    /\ ns' = [ns EXCEPT ![Owner[rop.from]][rop.from] = nextObj]
    /\ data' = [data EXCEPT ![nextObj] = data[staged]]
    /\ lid' = [lid EXCEPT ![nextObj] = lid[staged]]
    /\ bound' = IF StableHandles /\ Live(files[rop.from])
                THEN [bound EXCEPT ![files[rop.from]] = nextObj] ELSE bound
    /\ nextObj' = nextObj + 1 /\ rpc' = "abort"
    /\ UNCHANGED <<identity, hst, hat, hnat, hb, files, redir, cur, acked, nextW, renames, crashes, failures, rop, txn, staged, committed, published, denied, prior>>

Fail ==
    /\ rpc \in {"copy", "decide", "publish", "clear", "restore"} /\ failures < MaxFailures
    /\ failures' = failures + 1
    /\ rpc' = IF committed THEN rpc
              ELSE IF Ordered /\ rpc = "publish" THEN "restore" ELSE "abort"
    /\ UNCHANGED <<identity, ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir, bound, cur, acked, nextW, renames, crashes, rop, txn, staged, committed, published, denied, prior>>

Crash ==
    /\ crashes < MaxCrashes
    /\ crashes' = crashes + 1
    /\ ns' = [l \in Layers |-> IF l \in Durable THEN ns[l] ELSE ResetNs[l]]
    /\ data' = [o \in Objs |-> IF o \in ResetObjects THEN {} ELSE data[o]]
    /\ lid' = [o \in Objs |-> IF o \in ResetObjects THEN <<o, crashes + 1>> ELSE lid[o]]
    /\ hst' = [h \in Handles |-> IF hst[h] = "open" THEN "closed" ELSE hst[h]]
    /\ files' = [p \in Paths |-> NoB]
    /\ rpc' = IF txn = NoOp THEN "idle" ELSE IF committed THEN "publish" ELSE "abort"
    /\ rop' = txn
    /\ staged' = IF Journaled THEN staged ELSE 0
    /\ acked' = [h \in Handles |-> IF CrashPath(h) # NoPath /\ Owner[CrashPath(h)] \in Durable THEN acked[h] ELSE {}]
    /\ UNCHANGED <<identity, nextObj, hat, hnat, hb, redir, bound, cur, nextW, renames, failures, txn, committed, published, denied, prior>>


Finished == Idle /\ UNCHANGED vars
Progress == CopyToStaged \/ CommitDecision \/ PublishStaged \/ RemoveFrom \/ MoveBinding \/ Clear \/ Abort \/ Restore
Next == (\E h \in Handles : WriteAt(h) \/ (\E p \in OpenPaths : Open(h, p)))
        \/ (\E p \in CreatePaths \cup ReplacePaths : Create(p))
        \/ (\E op \in RenameOps : Rename(op[1], op[2]))
        \/ Progress \/ RemoveFailure \/ Fail \/ Crash \/ Finished
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
TypeOK ==
    /\ identity \in [Handles -> Objs \X (0..MaxCrashes)]
    /\ ns \in [Layers -> [Paths -> {0, DIR} \cup Objs]]
    /\ data \in [Objs -> SUBSET Writes]
    /\ lid \in [Objs -> Objs \X (0..MaxCrashes)]
    /\ hst \in [Handles -> {"unused", "open", "closed"}]
    /\ hat \in [Handles -> Paths \cup {NoPath}] /\ cur \in [Handles -> Paths \cup {NoPath}]
    /\ hnat \in [Handles -> {0} \cup Objs] /\ bound \in [Handles -> {0} \cup Objs]
    /\ hb \in [Handles -> Handles \cup {NoB}] /\ files \in [Paths -> Handles \cup {NoB}]
    /\ redir \in [Handles -> Paths \cup {NoPath}] /\ acked \in [Handles -> SUBSET Writes]
    /\ nextObj \in 1..(MaxObjs + 1) /\ nextW \in 1..(MaxWrites + 1)
    /\ renames \in 0..MaxRenames /\ crashes \in 0..MaxCrashes /\ failures \in 0..MaxFailures
    /\ rpc \in {"idle", "copy", "decide", "publish", "remove", "bind", "clear", "abort", "restore"}
    /\ staged \in {0} \cup Objs /\ committed \in BOOLEAN /\ published \in BOOLEAN
    /\ denied \subseteq Handles
AckedWritesVisible == Idle => \A h \in Handles :
    cur[h] # NoPath /\ acked[h] # {} => IsFile(cur[h]) /\ acked[h] \subseteq data[At(cur[h])]
HandleFollowsFile == Idle => \A h \in Handles :
    hst[h] = "open" /\ cur[h] # NoPath => Target(h) = At(cur[h])
OpenHandleIdentity == Idle => \A h \in Handles :
    hst[h] = "open" => Target(h) > 0 /\ lid[Target(h)] = identity[h]
NoDuplicateFile == Idle => \A p, q \in FilePaths :
    p # q /\ IsFile(p) /\ IsFile(q) => lid[At(p)] # lid[At(q)]
NoStagedLeftover == Idle => \A p \in StagedPaths : At(p) = 0
WritesRespectAccess == denied = {}
PreparedDestinationUnchanged ==
    txn # NoOp /\ ~committed /\ Owner[txn.to] \in Durable => At(txn.to) = prior.destination
JournalRetained == Journaled /\ rop.cross /\ rpc \notin {"idle", "abort"} => txn # NoOp
RecoveryEnabled == ~Idle => ENABLED Progress
RenameSettles == ~Idle ~> Idle
=============================================================================
