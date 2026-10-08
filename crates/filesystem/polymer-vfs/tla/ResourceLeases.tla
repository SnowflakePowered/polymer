--------------------------- MODULE ResourceLeases ---------------------------
(***************************************************************************)
(* T11: VFS resource leases across launches.                               *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   holder      resource/exclusive.rs ACTIVE_RESOURCES / ACTIVE_HOST_PATHS *)
(*               (owner is ghost; Rust stores only the key)                *)
(*   Ov          resource/exclusive.rs path_keys_overlap (abstract, K13)   *)
(*   Plan[l][i]  one binding's Resource::materialize lease: a              *)
(*               CompositeResourceLease built as                           *)
(*               exclusive(pre).with(tail).with(inner(exclusive(post)))    *)
(*               (polymer-runtime storage.rs SaveWatchFactory::materialize, *)
(*               backends/{memorycard,battery,nand,host}.rs)               *)
(*   acq[l][i]   ExclusiveResourceLease objects of binding i not yet       *)
(*               released (ExclusiveResourceLease::released)               *)
(*   tails[l]    live SaveWatchingLease watcher threads (storage.rs)       *)
(*   built[l]    bindings whose materialize returned Ok (pushed to the     *)
(*               local `leases` Vec in PreparedFilesystem::materialize)    *)
(*   inMR[l]     PreparedFilesystem::materialized is Some                  *)
(*   finalized   MaterializedResources::finalized                          *)
(*   fin[l][i]   ghost count of CompositeResourceLease::finalize calls     *)
(*   reason      FinalizationReason passed to the first finalize           *)
(*   ops[l]      pending lease releases of one finalize (reverse order,    *)
(*               MaterializedResources::finalize / Composite::finalize) or *)
(*               of one Drop of the local `leases` Vec (forward order)     *)
(*   coord       polymer-gui ShellCoordinator thread (launch_game,         *)
(*               finish_active_game run on it)                             *)
(*   session     SessionController::active                                 *)
(*   launched    EmulatorInstance::launch_with_input succeeded             *)
(*   pc[j]       polymer-runtime consolestorage jobs.rs / operations.rs    *)
(*               `_lease = ExclusiveResourceKey::Resource(id).acquire()`   *)
(*                                                                         *)
(* Actions: Begin/AcquireKey/AcquireConflict/FactoryFail/BindingDone/      *)
(* VolumeMountFail/MaterializeOk = PreparedFilesystem::materialize;        *)
(* HostStartFail = WinFspFilesystemHost::mount error arm;                  *)
(* SessionBusy/EarlyError = launch_game early returns dropping             *)
(* MountedFilesystem; SpawnFail/SpawnOk = SessionController::              *)
(* launch_with_input; AttachFail/AttachOk = GameHost::launch;              *)
(* ProcessExited = finish_active_game; Shutdown = SessionController Drop;  *)
(* Release = one lease release/stop step; DropPrepared = Drop of           *)
(* MountedFilesystem/PreparedFilesystem after finish.                      *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Launches, Jobs, Keys, OverlapRel, Plan, JobKeys, Free, None,
          MaxFailures

ASSUME OverlapRel \subseteq Keys \X Keys
ASSUME Free \notin Launches \cup Jobs /\ None \notin Launches

Ov(x, y) == x = y \/ <<x, y>> \in OverlapRel \/ <<y, x>> \in OverlapRel

Range(s) == {s[n] : n \in 1..Len(s)}
NB(l) == Len(Plan[l])
Bind(l, i) == Plan[l][i]
Seq_(l, i) == Bind(l, i).pre \o Bind(l, i).post
KeysOf(l, i) == Range(Seq_(l, i))
MaxB == 2
AnyKey == CHOOSE k \in Keys : TRUE

Rev(s) == [n \in 1..Len(s) |-> s[Len(s) - n + 1]]
KeyOps(i, s) == [n \in 1..Len(s) |-> [kind |-> "key", i |-> i, k |-> s[n]]]
TailOp(l, i) == IF Bind(l, i).tail THEN <<[kind |-> "tail", i |-> i, k |-> AnyKey]>> ELSE <<>>
FinOpsOf(l, i) == KeyOps(i, Rev(Bind(l, i).post)) \o TailOp(l, i) \o KeyOps(i, Rev(Bind(l, i).pre))
DropOpsOf(l, i) == KeyOps(i, Bind(l, i).pre) \o TailOp(l, i) \o KeyOps(i, Bind(l, i).post)

RECURSIVE FinOps(_, _), DropOps(_, _, _)
FinOps(l, i) == IF i = 0 THEN <<>> ELSE FinOpsOf(l, i) \o FinOps(l, i - 1)
DropOps(l, i, n) == IF i > n THEN <<>> ELSE DropOpsOf(l, i) \o DropOps(l, i + 1, n)

Holders == Launches \cup Jobs

VARIABLES holder, pc, bi, ki, acq, tails, built, inMR, finalized, fin,
          reason, ops, coord, session, launched, failures

vars == <<holder, pc, bi, ki, acq, tails, built, inMR, finalized, fin,
          reason, ops, coord, session, launched, failures>>

Conflicts(k) == \E k2 \in Keys : holder[k2] # Free /\ Ov(k, k2)

Init ==
  /\ holder = [k \in Keys |-> Free]
  /\ pc = [h \in Holders |-> "idle"]
  /\ bi = [l \in Launches |-> 1]
  /\ ki = [l \in Launches |-> 1]
  /\ acq = [l \in Launches |-> [i \in 1..MaxB |-> {}]]
  /\ tails = [l \in Launches |-> {}]
  /\ built = [l \in Launches |-> {}]
  /\ inMR = [l \in Launches |-> FALSE]
  /\ finalized = [l \in Launches |-> FALSE]
  /\ fin = [l \in Launches |-> [i \in 1..MaxB |-> 0]]
  /\ reason = [l \in Launches |-> "none"]
  /\ ops = [l \in Launches |-> <<>>]
  /\ coord = Free
  /\ session = None
  /\ launched = [l \in Launches |-> FALSE]
  /\ failures = 0

Unchanged(l) == UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, finalized,
                            fin, reason, ops, session, launched, failures>>

Begin(l) ==
  /\ pc[l] = "idle" /\ coord = Free
  /\ coord' = l /\ pc' = [pc EXCEPT ![l] = "acq"]
  /\ Unchanged(l)

AcquireKey(l) ==
  /\ pc[l] = "acq" /\ bi[l] <= NB(l) /\ ki[l] <= Len(Seq_(l, bi[l]))
  /\ LET k == Seq_(l, bi[l])[ki[l]] IN
       /\ ~Conflicts(k)
       /\ holder' = [holder EXCEPT ![k] = l]
       /\ acq' = [acq EXCEPT ![l][bi[l]] = @ \cup {k}]
  /\ ki' = [ki EXCEPT ![l] = @ + 1]
  /\ UNCHANGED <<pc, bi, tails, built, inMR, finalized, fin, reason, ops,
                 coord, session, launched, failures>>

AbortBinding(l) ==
  /\ holder' = [k \in Keys |-> IF k \in acq[l][bi[l]] THEN Free ELSE holder[k]]
  /\ acq' = [acq EXCEPT ![l][bi[l]] = {}]
  /\ ops' = [ops EXCEPT ![l] = DropOps(l, 1, bi[l] - 1)]
  /\ pc' = [pc EXCEPT ![l] = "unwind"]
  /\ UNCHANGED <<bi, ki, tails, built, inMR, finalized, fin, reason, coord,
                 session, launched>>

AcquireConflict(l) ==
  /\ pc[l] = "acq" /\ bi[l] <= NB(l) /\ ki[l] <= Len(Seq_(l, bi[l]))
  /\ Conflicts(Seq_(l, bi[l])[ki[l]])
  /\ AbortBinding(l)
  /\ UNCHANGED failures

FactoryFail(l) ==
  /\ pc[l] = "acq" /\ bi[l] <= NB(l) /\ ki[l] > Len(Seq_(l, bi[l]))
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ AbortBinding(l)

BindingDone(l) ==
  /\ pc[l] = "acq" /\ bi[l] <= NB(l) /\ ki[l] > Len(Seq_(l, bi[l]))
  /\ built' = [built EXCEPT ![l] = @ \cup {bi[l]}]
  /\ tails' = IF Bind(l, bi[l]).tail THEN [tails EXCEPT ![l] = @ \cup {bi[l]}] ELSE tails
  /\ bi' = [bi EXCEPT ![l] = @ + 1]
  /\ ki' = [ki EXCEPT ![l] = 1]
  /\ UNCHANGED <<holder, pc, acq, inMR, finalized, fin, reason, ops, coord,
                 session, launched, failures>>

VolumeMountFail(l) ==
  /\ pc[l] = "acq" /\ bi[l] > NB(l)
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ ops' = [ops EXCEPT ![l] = DropOps(l, 1, NB(l))]
  /\ pc' = [pc EXCEPT ![l] = "unwind"]
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, finalized, fin,
                 reason, coord, session, launched>>

MaterializeOk(l) ==
  /\ pc[l] = "acq" /\ bi[l] > NB(l)
  /\ inMR' = [inMR EXCEPT ![l] = TRUE]
  /\ pc' = [pc EXCEPT ![l] = "host"]
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, finalized, fin, reason,
                 ops, coord, session, launched, failures>>

Finalize(l, r) ==
  /\ IF inMR[l] /\ ~finalized[l]
       THEN /\ finalized' = [finalized EXCEPT ![l] = TRUE]
            /\ reason' = [reason EXCEPT ![l] = r]
            /\ fin' = [fin EXCEPT ![l] = [i \in 1..MaxB |->
                         IF i \in built[l] THEN fin[l][i] + 1 ELSE fin[l][i]]]
            /\ ops' = [ops EXCEPT ![l] = FinOps(l, NB(l))]
       ELSE UNCHANGED <<finalized, reason, fin, ops>>
  /\ pc' = [pc EXCEPT ![l] = "unwind"]

HostStartFail(l) ==
  /\ pc[l] = "host"
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ Finalize(l, "LaunchFailed")
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, session, launched>>

HostStartOk(l) ==
  /\ pc[l] = "host"
  /\ pc' = [pc EXCEPT ![l] = "checks"]
  /\ Unchanged(l) /\ UNCHANGED coord

SessionBusy(l) ==
  /\ pc[l] = "checks" /\ session # None
  /\ Finalize(l, "Cancelled")
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, session,
                 launched, failures>>

EarlyError(l) ==
  /\ pc[l] = "checks" /\ session = None
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ Finalize(l, "Cancelled")
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, session, launched>>

ChecksOk(l) ==
  /\ pc[l] = "checks" /\ session = None
  /\ pc' = [pc EXCEPT ![l] = "spawn"]
  /\ Unchanged(l) /\ UNCHANGED coord

SpawnFail(l) ==
  /\ pc[l] = "spawn"
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ Finalize(l, "LaunchFailed")
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, session, launched>>

SpawnOk(l) ==
  /\ pc[l] = "spawn"
  /\ session' = l
  /\ launched' = [launched EXCEPT ![l] = TRUE]
  /\ pc' = [pc EXCEPT ![l] = "attach"]
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, finalized, fin,
                 reason, ops, coord, failures>>

AttachFail(l) ==
  /\ pc[l] = "attach"
  /\ failures < MaxFailures /\ failures' = failures + 1
  /\ session' = None
  /\ Finalize(l, "LaunchFailed")
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, launched>>

AttachOk(l) ==
  /\ pc[l] = "attach"
  /\ coord' = Free
  /\ pc' = [pc EXCEPT ![l] = "running"]
  /\ Unchanged(l)

Stop(l, r) ==
  /\ pc[l] = "running" /\ coord = Free
  /\ coord' = l /\ session' = None
  /\ Finalize(l, r)
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, launched, failures>>

ProcessExited(l) == Stop(l, "ProcessExited")
Shutdown(l) == Stop(l, "Cancelled")

Release(l) ==
  /\ pc[l] = "unwind" /\ ops[l] # <<>>
  /\ LET op == Head(ops[l]) IN
       IF op.kind = "key"
         THEN /\ holder' = IF op.k \in acq[l][op.i]
                             THEN [holder EXCEPT ![op.k] = Free] ELSE holder
              /\ acq' = [acq EXCEPT ![l][op.i] = @ \ {op.k}]
              /\ UNCHANGED tails
         ELSE /\ tails' = [tails EXCEPT ![l] = @ \ {op.i}]
              /\ UNCHANGED <<holder, acq>>
  /\ ops' = [ops EXCEPT ![l] = Tail(@)]
  /\ UNCHANGED <<pc, bi, ki, built, inMR, finalized, fin, reason, coord,
                 session, launched, failures>>

Unwound(l) ==
  /\ pc[l] = "unwind" /\ ops[l] = <<>>
  /\ coord' = IF coord = l THEN Free ELSE coord
  /\ pc' = [pc EXCEPT ![l] = "end"]
  /\ Unchanged(l)

DropPrepared(l) ==
  /\ pc[l] = "end"
  /\ IF inMR[l] /\ ~finalized[l]
       THEN Finalize(l, "Cancelled")
       ELSE /\ pc' = [pc EXCEPT ![l] = "done"]
            /\ UNCHANGED <<finalized, reason, fin, ops>>
  /\ UNCHANGED <<holder, bi, ki, acq, tails, built, inMR, coord, session,
                 launched, failures>>

JobAcquire(j) ==
  /\ pc[j] = "idle"
  /\ \E k \in JobKeys :
       IF Conflicts(k)
         THEN /\ pc' = [pc EXCEPT ![j] = "done"] /\ UNCHANGED holder
         ELSE /\ holder' = [holder EXCEPT ![k] = j]
              /\ pc' = [pc EXCEPT ![j] = "work"]
  /\ UNCHANGED <<bi, ki, acq, tails, built, inMR, finalized, fin, reason, ops,
                 coord, session, launched, failures>>

JobDrop(j) ==
  /\ pc[j] = "work"
  /\ holder' = [k \in Keys |-> IF holder[k] = j THEN Free ELSE holder[k]]
  /\ pc' = [pc EXCEPT ![j] = "done"]
  /\ UNCHANGED <<bi, ki, acq, tails, built, inMR, finalized, fin, reason, ops,
                 coord, session, launched, failures>>

Finished ==
  /\ \A h \in Holders : pc[h] = "done"
  /\ UNCHANGED vars

LaunchStep(l) ==
  \/ Begin(l) \/ AcquireKey(l) \/ AcquireConflict(l) \/ FactoryFail(l)
  \/ BindingDone(l) \/ VolumeMountFail(l) \/ MaterializeOk(l)
  \/ HostStartFail(l) \/ HostStartOk(l)
  \/ SessionBusy(l) \/ EarlyError(l) \/ ChecksOk(l)
  \/ SpawnFail(l) \/ SpawnOk(l) \/ AttachFail(l) \/ AttachOk(l)
  \/ ProcessExited(l) \/ Shutdown(l)
  \/ Release(l) \/ Unwound(l) \/ DropPrepared(l)

JobStep(j) == JobAcquire(j) \/ JobDrop(j)

Next ==
  \/ \E l \in Launches : LaunchStep(l)
  \/ \E j \in Jobs : JobStep(j)
  \/ Finished

Spec == Init /\ [][Next]_vars
          /\ \A l \in Launches : SF_vars(LaunchStep(l))
          /\ \A j \in Jobs : WF_vars(JobStep(j))

-----------------------------------------------------------------------------
TypeOK ==
  /\ holder \in [Keys -> Holders \cup {Free}]
  /\ coord \in Launches \cup {Free}
  /\ session \in Launches \cup {None}
  /\ \A l \in Launches : built[l] \subseteq 1..NB(l) /\ tails[l] \subseteq built[l]
  /\ \A l \in Launches : \A i \in 1..MaxB : \A k \in acq[l][i] : holder[k] = l

NoOverlappingHolders ==
  \A k1, k2 \in Keys :
    (holder[k1] # Free /\ holder[k2] # Free /\ holder[k1] # holder[k2])
      => ~Ov(k1, k2)

HeldWhileRunning ==
  \A l \in Launches : pc[l] = "running" =>
    \A i \in built[l] : \A k \in KeysOf(l, i) : holder[k] = l

WatcherProtected ==
  \A l \in Launches : \A i \in tails[l] : \A k \in KeysOf(l, i) :
    \A k2 \in Keys : holder[k2] \notin {l, Free} => ~Ov(k, k2)

FinalizeAtMostOnce ==
  \A l \in Launches : \A i \in 1..MaxB : fin[l][i] <= 1

FinalizedExactlyOnce ==
  \A l \in Launches : pc[l] = "done" => \A i \in built[l] : fin[l][i] = 1

ReleasedOnExit ==
  \A l \in Launches : pc[l] = "done" =>
    /\ \A k \in Keys : holder[k] # l
    /\ tails[l] = {}

ReasonFaithful ==
  \A l \in Launches : (pc[l] = "done" /\ finalized[l] /\ ~launched[l])
    => reason[l] = "LaunchFailed"

AllTerminate == <>(\A h \in Holders : pc[h] = "done")

AllReleased == <>[](\A k \in Keys : holder[k] = Free)
=============================================================================
