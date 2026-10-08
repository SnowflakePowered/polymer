--------------------------- MODULE ComposedRename ---------------------------
(***************************************************************************)
(* T12: composed storage cross-layer rename.                              *)
(*                                                                         *)
(* Rust mapping (src/storage/composed.rs unless noted):                    *)
(*   ns[l][p]   entry at path p in layer l (ComposedStorage.layers[l]);    *)
(*              0 = absent, DIR = directory, n > 0 = file object n.        *)
(*              Owner[p] is MountableStorage::claims (first claimer).      *)
(*   data[o]    bytes of file object o, as a set of write tokens           *)
(*              (MemoryFile / native inode; handles keep an unlinked or    *)
(*              replaced object alive and writable).                       *)
(*   lid[o]     ghost: logical file an object is a copy of.                *)
(*   hst[h]     ComposedFile lifetime ("unused" / "open" / "closed").      *)
(*   hat[h]     ComposedFile.opened_at                                     *)
(*   hnat[h]    ComposedFile.handle (the layer's native handle -> object)  *)
(*   hb[h]      ComposedFile.binding (binding id = handle that created it) *)
(*   files[p]   ComposedStorage.files (Weak; live iff an open handle       *)
(*              holds the binding)                                         *)
(*   redir[b]   ComposedFileBinding.redirected_to                          *)
(*   rpc, rop   one in-flight MountableStorage::rename(from, to)           *)
(*   cur[h]     ghost: the path of h's file, moved when the layer's native *)
(*              rename (same layer) or the staged publish (cross layer)    *)
(*              makes the new name visible                                 *)
(*   acked[h]   ghost: write tokens whose write_at through h returned Ok   *)
(*                                                                         *)
(* Actions:                                                                *)
(*   Open          Storage::create / open + ComposedStorage::file/binding  *)
(*   Create        guest create of a file through a handle it drops        *)
(*   WriteAt       ComposedFile::write_at via ComposedFile::redirected     *)
(*   Rename        rename(): owner lookup, same-layer native rename, or    *)
(*                 cross-layer metadata check                              *)
(*   CopyToStaged  rename(): open(from), create(staged), copy loop, sync   *)
(*                 (the copy loop is one snapshot step)                    *)
(*   PublishStaged rename(): to_owner.rename(staged, to)                   *)
(*   RemoveFrom    rename(): let _ = from_owner.remove(from) (may fail)    *)
(*   MoveBinding   move_binding(from, to, crossed_layers), then Ok(())     *)
(*   Crash         process death: volatile layers (ScratchStorage upper)   *)
(*                 reset, handles / files map / in-flight rename lost      *)
(*                                                                         *)
(* Abstracted: create over an existing file (only creates absent files),   *)
(* handle close, read/set_len/flush/sync (same redirect as write_at),      *)
(* copy-loop failures, claims() depending on existence, and opening a     *)
(* path that an in-flight rename is moving (no linearization point).      *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Layers, Durable, Paths, Owner, FilePaths, RenameOps, InitNs,
          Handles, MaxObjs, MaxWrites, MaxRenames, MaxCrashes, MaxFailures

DIR == -1
NoPath == <<>>
NoB == "nob"
Objs == 1..MaxObjs
Writes == 1..MaxWrites

ASSUME Owner \in [Paths -> Layers] /\ Durable \subseteq Layers
ASSUME InitNs \in [Layers -> [Paths -> {0, DIR} \cup Objs]]

NoOp == [from |-> NoPath, to |-> NoPath, cross |-> FALSE]

VARIABLES ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir,
          rpc, rop, cur, acked, nextW, renames, crashes, failures

vars == <<ns, data, lid, nextObj, hst, hat, hnat, hb, files, redir,
          rpc, rop, cur, acked, nextW, renames, crashes, failures>>

Parent(p) == SubSeq(p, 1, Len(p) - 1)
Staged(to) == Parent(to) \o <<"~" \o to[Len(to)]>>
StagedPaths == {Staged(p) : p \in FilePaths} \cap Paths
Under(p, root) == Len(p) >= Len(root) /\ SubSeq(p, 1, Len(root)) = root
Rebase(p, from, to) == to \o SubSeq(p, Len(from) + 1, Len(p))
At(p) == ns[Owner[p]][p]
IsFile(p) == At(p) > 0

Live(b) == b # NoB /\ \E h \in Handles : hst[h] = "open" /\ hb[h] = b

\* ComposedFile::redirected + the layer open(path, true) it falls through to.
Target(h) ==
  LET r == redir[hb[h]] IN
  IF r = NoPath \/ r = hat[h] THEN hnat[h]
  ELSE IF ns[Owner[r]][r] > 0 THEN ns[Owner[r]][r] ELSE 0

WithParent(m, p) == IF Len(p) > 1 THEN [m EXCEPT ![Parent(p)] = DIR] ELSE m
ParentOk(m, p) == IF Len(p) = 1 THEN TRUE ELSE m[Parent(p)] \in {0, DIR}

TypeOK ==
  /\ ns \in [Layers -> [Paths -> {0, DIR} \cup Objs]]
  /\ data \in [Objs -> SUBSET Writes]
  /\ hst \in [Handles -> {"unused", "open", "closed"}]
  /\ files \in [Paths -> Handles \cup {NoB}]
  /\ rpc \in {"idle", "copy", "publish", "remove", "bind"}

Init ==
  /\ ns = InitNs
  /\ nextObj = 1 + Cardinality({o \in Objs : \E l \in Layers, p \in Paths : InitNs[l][p] = o})
  /\ data = [o \in Objs |-> {}]
  /\ lid = [o \in Objs |-> o]
  /\ hst = [h \in Handles |-> "unused"]
  /\ hat = [h \in Handles |-> NoPath]
  /\ hnat = [h \in Handles |-> 0]
  /\ hb = [h \in Handles |-> NoB]
  /\ files = [p \in Paths |-> NoB]
  /\ redir = [h \in Handles |-> NoPath]
  /\ rpc = "idle" /\ rop = NoOp
  /\ cur = [h \in Handles |-> NoPath]
  /\ acked = [h \in Handles |-> {}]
  /\ nextW = 1 /\ renames = 0 /\ crashes = 0 /\ failures = 0

NewFile(p) ==
  LET l == Owner[p] IN
  /\ At(p) = 0 /\ nextObj <= MaxObjs /\ ParentOk(ns[l], p)
  /\ ns' = [ns EXCEPT ![l] = [WithParent(@, p) EXCEPT ![p] = nextObj]]
  /\ nextObj' = nextObj + 1

Open(h, p) ==
  /\ hst[h] = "unused" /\ p \in FilePaths
  /\ rpc = "idle" \/ ~(Under(p, rop.from) \/ Under(p, rop.to))
  /\ \/ /\ IsFile(p) /\ hnat' = [hnat EXCEPT ![h] = At(p)]
        /\ UNCHANGED <<ns, nextObj>>
     \/ /\ NewFile(p) /\ hnat' = [hnat EXCEPT ![h] = nextObj]
  /\ hst' = [hst EXCEPT ![h] = "open"]
  /\ hat' = [hat EXCEPT ![h] = p]
  /\ cur' = [cur EXCEPT ![h] = p]
  /\ IF Live(files[p])
       THEN /\ hb' = [hb EXCEPT ![h] = files[p]] /\ UNCHANGED <<files, redir>>
       ELSE /\ hb' = [hb EXCEPT ![h] = h]
            /\ files' = [files EXCEPT ![p] = h]
            /\ redir' = [redir EXCEPT ![h] = NoPath]
  /\ UNCHANGED <<data, lid, rpc, rop, acked, nextW, renames, crashes, failures>>

Create(p) ==
  /\ p \in FilePaths /\ NewFile(p)
  /\ UNCHANGED <<data, lid, hst, hat, hnat, hb, files, redir, rpc, rop, cur, acked,
                 nextW, renames, crashes, failures>>

WriteAt(h) ==
  /\ hst[h] = "open" /\ nextW <= MaxWrites /\ Target(h) > 0
  /\ data' = [data EXCEPT ![Target(h)] = @ \cup {nextW}]
  /\ acked' = [acked EXCEPT ![h] = @ \cup {nextW}]
  /\ nextW' = nextW + 1
  /\ UNCHANGED <<ns, lid, nextObj, hst, hat, hnat, hb, files, redir, rpc, rop, cur,
                 renames, crashes, failures>>

\* Native same-layer rename (MemoryStorage / DirectoryStorage / git): moves
\* the subtree and replaces whatever was at `to`.
NativeRename(m, from, to) ==
  [p \in Paths |->
     IF Under(p, from) THEN 0
     ELSE IF \E q \in Paths : Under(q, from) /\ Rebase(q, from, to) = p
       THEN m[CHOOSE q \in Paths : Under(q, from) /\ Rebase(q, from, to) = p]
     ELSE IF Under(p, to) THEN 0
     ELSE IF Len(to) > 1 /\ p = Parent(to) THEN DIR
     ELSE m[p]]

AckedPath(p, from, to) ==
  IF Under(p, to) THEN NoPath
  ELSE IF Under(p, from) THEN Rebase(p, from, to)
  ELSE p

Rename(from, to) ==
  /\ rpc = "idle" /\ renames < MaxRenames /\ <<from, to>> \in RenameOps
  /\ renames' = renames + 1
  /\ IF Owner[from] = Owner[to]
       THEN IF At(from) # 0 /\ ParentOk(ns[Owner[to]], to)
              THEN /\ ns' = [ns EXCEPT ![Owner[from]] = NativeRename(@, from, to)]
                   /\ rpc' = "bind"
                   /\ rop' = [from |-> from, to |-> to, cross |-> FALSE]
                   /\ cur' = [h \in Handles |-> AckedPath(cur[h], from, to)]
              ELSE UNCHANGED <<ns, rpc, rop, cur>>
       ELSE IF IsFile(from)
              THEN /\ rpc' = "copy"
                   /\ rop' = [from |-> from, to |-> to, cross |-> TRUE]
                   /\ UNCHANGED <<ns, cur>>
              ELSE UNCHANGED <<ns, rpc, rop, cur>>
  /\ UNCHANGED <<data, lid, nextObj, hst, hat, hnat, hb, files, redir, acked,
                 nextW, crashes, failures>>

CopyToStaged ==
  LET from == rop.from
      s == Staged(rop.to)
      l == Owner[rop.to]
  IN
  /\ rpc = "copy"
  /\ IF IsFile(from) /\ nextObj <= MaxObjs /\ ParentOk(ns[l], s)
       THEN /\ ns' = [ns EXCEPT ![l] = [WithParent(@, s) EXCEPT ![s] = nextObj]]
            /\ data' = [data EXCEPT ![nextObj] = data[At(from)]]
            /\ lid' = [lid EXCEPT ![nextObj] = lid[At(from)]]
            /\ nextObj' = nextObj + 1
            /\ rpc' = "publish"
            /\ UNCHANGED rop
       ELSE /\ rpc' = "idle" /\ rop' = NoOp
            /\ UNCHANGED <<ns, data, lid, nextObj>>
  /\ UNCHANGED <<hst, hat, hnat, hb, files, redir, cur, acked, nextW, renames,
                 crashes, failures>>

PublishStaged ==
  LET l == Owner[rop.to] IN
  /\ rpc = "publish"
  /\ ns' = [ns EXCEPT ![l] = NativeRename(@, Staged(rop.to), rop.to)]
  /\ rpc' = "remove"
  /\ cur' = [h \in Handles |-> AckedPath(cur[h], rop.from, rop.to)]
  /\ UNCHANGED <<data, lid, nextObj, hst, hat, hnat, hb, files, redir, rop, acked,
                 nextW, renames, crashes, failures>>

RemoveFrom ==
  /\ rpc = "remove"
  /\ \/ ns' = [ns EXCEPT ![Owner[rop.from]][rop.from] = 0]
        /\ UNCHANGED failures
     \/ failures < MaxFailures /\ failures' = failures + 1
        /\ UNCHANGED ns
  /\ rpc' = "bind"
  /\ UNCHANGED <<data, lid, nextObj, hst, hat, hnat, hb, files, redir, rop, cur, acked,
                 nextW, renames, crashes>>

MoveBinding ==
  LET from == rop.from
      to == rop.to
      src == IF Live(files[from]) THEN files[from] ELSE NoB
  IN
  /\ rpc = "bind"
  /\ files' = [files EXCEPT ![from] = NoB, ![to] = src]
  /\ redir' = IF src # NoB /\ (rop.cross \/ redir[src] # NoPath)
                THEN [redir EXCEPT ![src] = to] ELSE redir
  /\ rpc' = "idle" /\ rop' = NoOp
  /\ UNCHANGED <<ns, data, lid, nextObj, hst, hat, hnat, hb, cur, acked, nextW, renames,
                 crashes, failures>>

Crash ==
  /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ ns' = [l \in Layers |-> IF l \in Durable THEN ns[l] ELSE InitNs[l]]
  /\ hst' = [h \in Handles |-> IF hst[h] = "open" THEN "closed" ELSE hst[h]]
  /\ files' = [p \in Paths |-> NoB]
  /\ rpc' = "idle" /\ rop' = NoOp
  /\ acked' = [h \in Handles |->
                 IF cur[h] # NoPath /\ Owner[cur[h]] \in Durable THEN acked[h] ELSE {}]
  /\ UNCHANGED <<data, lid, nextObj, hat, hnat, hb, redir, cur, nextW, renames, failures>>

Finished == rpc = "idle" /\ UNCHANGED vars

Next ==
  \/ \E h \in Handles : WriteAt(h) \/ \E p \in Paths : Open(h, p)
  \/ \E p \in Paths : Create(p)
  \/ \E op \in RenameOps : Rename(op[1], op[2])
  \/ CopyToStaged \/ PublishStaged \/ RemoveFrom \/ MoveBinding
  \/ Crash
  \/ Finished

Spec == Init /\ [][Next]_vars

Idle == rpc = "idle"

\* Every acknowledged write is visible at the file's current path.
AckedWritesVisible ==
  Idle => \A h \in Handles :
            cur[h] # NoPath => \A w \in acked[h] : IsFile(cur[h]) /\ w \in data[At(cur[h])]

\* An open handle's next write would land in the file at its current path.
HandleFollowsFile ==
  Idle => \A h \in Handles :
            hst[h] = "open" /\ cur[h] # NoPath => Target(h) = At(cur[h])

\* The union listing never shows one logical file under two names.
NoDuplicateFile ==
  Idle => \A p, q \in Paths \ StagedPaths :
            p # q /\ IsFile(p) /\ IsFile(q) => lid[At(p)] # lid[At(q)]

\* No staging file is left in the union listing.
NoStagedLeftover == Idle => \A p \in StagedPaths : At(p) = 0
=============================================================================
