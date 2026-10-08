---------------------------- MODULE NandRewrite ----------------------------
(***************************************************************************)
(* NAND uninstall as a rebase of the linear NAND history.                  *)
(*                                                                         *)
(* Rust mapping (src/storage/git/registry.rs, artifact/history.rs,         *)
(* artifact/record.rs, artifact/mod.rs, commit.rs):                        *)
(*   hist            linear_history(tip): Vec<NandCommit>, oldest first;   *)
(*                   commit.art = artifact_from_message ("Polymer install  *)
(*                   <id>" from GitArtifactRegistry::record, NoArt for     *)
(*                   commit_pending saves); commit.tree = tree_snapshot    *)
(*   tree[p], p \in FilePaths   files/<p> leaf (blob id ~ Vals)            *)
(*   tree[i], i \in Ids         meta/artifacts/<TreeKey(i)>/ subtree,      *)
(*                   valued by the ArtifactRecord manifest it encodes      *)
(*                   ([FilePaths -> Vals \cup {Keep}], Keep = not listed)  *)
(*   req             ids passed to GitArtifactRegistry::uninstall          *)
(*   res             outcome of uninstall: ok + rewritten history, or the  *)
(*                   Error it returns                                      *)
(* Operators: ApplyDelta = apply_delta, RetainDelta = retain_delta,        *)
(*   ManifestDependencies = manifest_dependencies, RetainedFiles =         *)
(*   retained_files, AffectedDepots = affected_depots, RewriteWithout =    *)
(*   rewrite_without, Uninstall = GitArtifactRegistry::uninstall,          *)
(*   Records = records() read from the tip's meta/artifacts.               *)
(* Install commits (record -> commit_pending_reporting) write every        *)
(* manifest file with its manifest content plus meta/<id>; save commits    *)
(* (commit_pending) change files/ only and are only created when the tree  *)
(* changes. Ref publication, reflog clearing, checkout refresh, working    *)
(* overlay and the operation lock are abstracted away.                     *)
(* Every history starts with the brand() "Polymer volume created" commit.  *)
(* Non-install NAND commits are brand/set_label (commit_pending); NAND has *)
(* no autosave, so their files/ changes are pending overlay writes swept  *)
(* in. They are modelled as generic "save" commits changing one path.     *)
(* Reference semantics (RefTree / RefMustError), per files/<p>:            *)
(*   not retained: replay p's writers without removed installs; must error *)
(*     if a kept commit wrote p on top of a removed install's write and   *)
(*     neither its parent nor its value equals the rebased predecessor;    *)
(*   removed writes are only the manifest-listed ones of a removed id's    *)
(*     install commits (Bundled: other pending writes swept into an       *)
(*     install commit count as guest writes);                              *)
(*   retained (listed by a remaining title): keep tip unless the last      *)
(*     writer was removed, then the remaining titles' listed content; must *)
(*     error if those listings disagree.                                   *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Ids, FilePaths, Vals, Absent, Keep, NoArt, MaxLen, Bundled

Paths == FilePaths \cup Ids
Manifests == [FilePaths -> Vals \cup {Keep}]
EmptyTree == [p \in Paths |-> Absent]

Max(S) == CHOOSE m \in S : \A n \in S : n <= m
Min(S) == CHOOSE m \in S : \A n \in S : m <= n

-----------------------------------------------------------------------------
(* History generation *)

NoExtra == [p \in FilePaths |-> Keep]
Extras(m) ==
    {NoExtra} \cup
    IF Bundled
    THEN {[q \in FilePaths |-> IF q = p THEN v ELSE Keep] :
             p \in {f \in FilePaths : m[f] = Keep}, v \in Vals \cup {Absent}}
    ELSE {}
InstallSteps ==
    UNION {{[kind |-> "install", art |-> i, delta |-> m, extra |-> e] : e \in Extras(m)} :
              i \in Ids, m \in Manifests}
SaveSteps == {[kind |-> "save", art |-> NoArt,
               delta |-> [q \in FilePaths |-> IF q = p THEN v ELSE Keep],
               extra |-> NoExtra] :
               p \in FilePaths, v \in Vals \cup {Absent}}
Steps == InstallSteps \cup SaveSteps

StepTree(prev, s) ==
    [p \in Paths |->
        IF p \in Ids
        THEN IF s.kind = "install" /\ s.art = p THEN s.delta ELSE prev[p]
        ELSE IF s.delta[p] # Keep THEN s.delta[p]
        ELSE IF s.extra[p] # Keep THEN s.extra[p]
        ELSE prev[p]]

RECURSIVE Build(_, _, _)
Build(steps, i, prev) ==
    IF i > Len(steps) THEN <<>>
    ELSE LET t == StepTree(prev, steps[i])
         IN <<[art |-> steps[i].art, tree |-> t]>> \o Build(steps, i + 1, t)

ValidHistory(h) ==
    \A i \in 2..Len(h) : h[i].art = NoArt => h[i].tree # h[i-1].tree

Histories ==
    {h \in {<<[art |-> NoArt, tree |-> EmptyTree]>> \o Build(s, 1, EmptyTree) :
              s \in UNION {[1..n -> Steps] : n \in 1..MaxLen}} :
        ValidHistory(h)}

-----------------------------------------------------------------------------
(* Code-faithful operators *)

Parent(h, i) == IF i = 1 THEN EmptyTree ELSE h[i-1].tree
Tip(h) == h[Len(h)].tree
Installed(tip) == {i \in Ids : tip[i] # Absent}
Lists(tip, r, p) == tip[r][p] # Keep

ApplyDelta(before, after, cur, removed) ==
    LET changed == {p \in Paths : before[p] # after[p] /\ p \notin removed}
        conflicts == {p \in changed : cur[p] # before[p] /\ cur[p] # after[p]}
    IN [conflicts |-> conflicts,
        tree |-> IF conflicts = {}
                 THEN [p \in Paths |-> IF p \in changed THEN after[p] ELSE cur[p]]
                 ELSE cur]

RetainDelta(before, after, cur, retained) ==
    [p \in Paths |-> IF p \in retained /\ before[p] # after[p] THEN after[p] ELSE cur[p]]

ManifestDependencies(tip, selected) ==
    {r \in Installed(tip) \ selected :
        \E p \in FilePaths : Lists(tip, r, p) /\
            \E s \in selected \cap Installed(tip) :
                Lists(tip, s, p) /\ tip[s][p] # tip[r][p]}

RetainedFiles(tip, removed) ==
    {p \in FilePaths : \E r \in Installed(tip) \ removed : Lists(tip, r, p)}

FirstInstall(h, sel) == Min({i \in 1..Len(h) : h[i].art \in sel})

RECURSIVE AffectedReplay(_, _, _, _, _)
AffectedReplay(h, sel, retained, i, cur) ==
    IF i > Len(h) THEN [status |-> "done", add |-> NoArt]
    ELSE IF h[i].art \in sel
         THEN AffectedReplay(h, sel, retained, i + 1,
                             RetainDelta(Parent(h, i), h[i].tree, cur, retained))
         ELSE LET r == ApplyDelta(Parent(h, i), h[i].tree, cur, sel)
              IN IF r.conflicts = {}
                 THEN AffectedReplay(h, sel, retained, i + 1, r.tree)
                 ELSE IF h[i].art # NoArt
                      THEN [status |-> "added", add |-> h[i].art]
                      ELSE [status |-> "conflict", add |-> NoArt]

RECURSIVE AffectedLoop(_, _, _, _)
AffectedLoop(h, tip, requested, sel) ==
    IF \E s \in sel : \A i \in 1..Len(h) : h[i].art # s
    THEN [ok |-> FALSE, err |-> "missing", ids |-> {}]
    ELSE LET deps == ManifestDependencies(tip, sel)
             run == AffectedReplay(h, sel, RetainedFiles(tip, sel), FirstInstall(h, sel),
                                   Parent(h, FirstInstall(h, sel)))
             added == (deps \cup (IF run.status = "added" THEN {run.add} ELSE {})) \ sel
         IN IF run.status = "conflict"
            THEN [ok |-> FALSE, err |-> "affected-conflict", ids |-> {}]
            ELSE IF added = {}
                 THEN [ok |-> TRUE, err |-> "", ids |-> sel \ requested]
                 ELSE AffectedLoop(h, tip, requested, sel \cup added)

AffectedDepots(h, tip, requested) ==
    IF requested \cap Installed(tip) = {}
    THEN [ok |-> TRUE, err |-> "", ids |-> {}]
    ELSE AffectedLoop(h, tip, requested, requested \cap Installed(tip))

RECURSIVE RewriteLoop(_, _, _, _, _, _)
RewriteLoop(h, removed, retained, i, cur, out) ==
    IF i > Len(h) THEN [ok |-> TRUE, err |-> "", hist |-> out]
    ELSE IF h[i].art \in removed
         THEN RewriteLoop(h, removed, retained, i + 1,
                          RetainDelta(Parent(h, i), h[i].tree, cur, retained), out)
         ELSE LET r == ApplyDelta(Parent(h, i), h[i].tree, cur, removed)
              IN IF r.conflicts # {}
                 THEN [ok |-> FALSE, err |-> "rewrite-conflict", hist |-> <<>>]
                 ELSE RewriteLoop(h, removed, retained, i + 1, r.tree,
                                  Append(out, [art |-> h[i].art, tree |-> r.tree]))

RewriteWithout(h, removed, retained) ==
    LET first == FirstInstall(h, removed)
        run == RewriteLoop(h, removed, retained, first, Parent(h, first),
                           SubSeq(h, 1, first - 1))
    IN IF ~run.ok THEN run
       ELSE IF run.hist = <<>>
            THEN [ok |-> FALSE, err |-> "entire", hist |-> <<>>]
            ELSE IF \E r \in removed : Tip(run.hist)[r] # Absent
                 THEN [ok |-> FALSE, err |-> "still-records", hist |-> <<>>]
                 ELSE run

Uninstall(h, requested) ==
    LET tip == Tip(h)
        removed == requested \cap Installed(tip)
        affected == AffectedDepots(h, tip, removed)
    IN IF removed = {} THEN [ok |-> TRUE, err |-> "", hist |-> h]
       ELSE IF ~affected.ok THEN [ok |-> FALSE, err |-> affected.err, hist |-> <<>>]
       ELSE IF affected.ids # {} THEN [ok |-> FALSE, err |-> "affected", hist |-> <<>>]
       ELSE RewriteWithout(h, removed, RetainedFiles(tip, removed))

-----------------------------------------------------------------------------
(* Reference semantics *)

Writers(h, p) == {i \in 1..Len(h) : Parent(h, i)[p] # h[i].tree[p]}
RemovedIdx(h, removed, p) ==
    {i \in 1..Len(h) : h[i].art \in removed /\ h[i].tree[h[i].art][p] # Keep}

ReplayVal(h, removed, p) ==
    LET kept == Writers(h, p) \ RemovedIdx(h, removed, p)
    IN IF kept = {} THEN Absent ELSE h[Max(kept)].tree[p]

RebasedBefore(h, removed, p, j) ==
    LET kept == {k \in Writers(h, p) \ RemovedIdx(h, removed, p) : k < j}
    IN IF kept = {} THEN Absent ELSE h[Max(kept)].tree[p]

GuestWrite(h, i, p) == h[i].art = NoArt \/ h[i].tree[h[i].art][p] = Keep

LastWriterRemoved(h, removed, p) ==
    Writers(h, p) # {} /\ Max(Writers(h, p)) \in RemovedIdx(h, removed, p)

Listed(tip, removed, p) ==
    {tip[r][p] : r \in {r \in Installed(tip) \ removed : Lists(tip, r, p)}}

RefMustError(h, removed) ==
    LET tip == Tip(h)
        retained == RetainedFiles(tip, removed)
    IN \/ \E p \in FilePaths \ retained :
            \E j \in Writers(h, p) \ RemovedIdx(h, removed, p) :
                /\ \E i \in Writers(h, p) \cap RemovedIdx(h, removed, p) :
                      /\ i < j
                      /\ \A k \in Writers(h, p) \ RemovedIdx(h, removed, p) : k < j => k < i
                /\ Parent(h, j)[p] # RebasedBefore(h, removed, p, j)
                /\ h[j].tree[p] # RebasedBefore(h, removed, p, j)
       \/ \E p \in retained :
            LastWriterRemoved(h, removed, p) /\ Cardinality(Listed(tip, removed, p)) > 1

RefTree(h, removed) ==
    LET tip == Tip(h)
        retained == RetainedFiles(tip, removed)
    IN [p \in Paths |->
          IF p \in Ids THEN IF p \in removed THEN Absent ELSE tip[p]
          ELSE IF p \in retained
               THEN IF LastWriterRemoved(h, removed, p)
                    THEN CHOOSE v \in Listed(tip, removed, p) : TRUE
                    ELSE tip[p]
          ELSE ReplayVal(h, removed, p)]

-----------------------------------------------------------------------------

VARIABLES hist, req, res

vars == <<hist, req, res>>

Init ==
    /\ hist \in Histories
    /\ req \in (SUBSET Ids) \ {{}}
    /\ res = Uninstall(hist, req)

Next == UNCHANGED vars

Spec == Init /\ [][Next]_vars

Removed == req \cap Installed(Tip(hist))
NewTip == Tip(res.hist)
OldTip == Tip(hist)

SavesPreserved ==
    res.ok => \A p \in FilePaths :
        (Writers(hist, p) # {} /\ GuestWrite(hist, Max(Writers(hist, p)), p))
            => NewTip[p] = OldTip[p]

RemovalComplete ==
    res.ok =>
        /\ \A r \in Removed : NewTip[r] = Absent
        /\ \A p \in FilePaths \ RetainedFiles(OldTip, Removed) :
              (Writers(hist, p) # {} /\ Writers(hist, p) \subseteq RemovedIdx(hist, Removed, p))
                  => NewTip[p] = Absent

RemovalConflictErrors == RefMustError(hist, Removed) => ~res.ok

SharedContentSurvives ==
    res.ok => \A p \in RetainedFiles(OldTip, Removed) :
        OldTip[p] \in Listed(OldTip, Removed, p) => NewTip[p] = OldTip[p]

UntouchedPreserved ==
    res.ok =>
        /\ \A p \in FilePaths :
              Writers(hist, p) \cap RemovedIdx(hist, Removed, p) = {} => NewTip[p] = OldTip[p]
        /\ \A i \in Ids \ Removed : NewTip[i] = OldTip[i]

Sequential(one) ==
    LET first == Uninstall(hist, {one})
    IN IF first.ok THEN Uninstall(first.hist, Ids \ {one}) ELSE first

SequentialMatchesBatch ==
    req = Ids =>
        \A one \in Ids :
            (res.ok /\ Sequential(one).ok) => Tip(Sequential(one).hist) = NewTip

OrderIndependent ==
    req = Ids =>
        \A one, two \in Ids :
            (Sequential(one).ok /\ Sequential(two).ok)
                => Tip(Sequential(one).hist) = Tip(Sequential(two).hist)

AffectedAgreesWithRewrite == res.err # "rewrite-conflict"

NoSilentLoss == res.ok => (~RefMustError(hist, Removed) /\ NewTip = RefTree(hist, Removed))

NoSpuriousError == ~res.ok => RefMustError(hist, Removed)

NoSpuriousConflict == (~res.ok /\ res.err # "affected") => RefMustError(hist, Removed)

=============================================================================
