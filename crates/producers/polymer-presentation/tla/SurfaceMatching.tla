---------------------------- MODULE SurfaceMatching ----------------------------
(***************************************************************************)
(* T2: surface matching (swapchain/matching.rs).                          *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   cands          Assignments::candidates (Vec, order matters for find)  *)
(*   req[r]         Assignments::requests[r] {previous_window, binding,    *)
(*                  committed}; req[r].b is the Arc<FixedSurfaceBinding>,  *)
(*                  compared by Arc::ptr_eq as an index into bs            *)
(*   bs             every FixedSurfaceBinding allocated by reserve()       *)
(*                  (its generation is its index)                          *)
(*   inst[i]        one hooked source: D3D12 creation.rs / Vulkan          *)
(*                  surface/mod.rs Surface owning a SourceRegistration     *)
(*   prov[i]        ProvisionalBinding held by the source                  *)
(*   held[i]        committed Arc<FixedSurfaceBinding> held by the source  *)
(*   win[h]         WindowLifetime::identify serial of the live HWND h     *)
(*                  (0 = no window)                                        *)
(*   foreignForget  ghost: forget(source) issued by a source whose         *)
(*                  identity is now owned by another live registration     *)
(*                                                                         *)
(* SourceIdentity: D3D12 uses a process-wide serial (never repeats); the   *)
(* Vulkan layer uses (instance, VkSurfaceKHR handle, window lifetime), so  *)
(* a recreated surface on the same window can repeat it (IdentityReuse).   *)
(* Title patterns are abstracted away: every request is unqualified, so    *)
(* resolve() runs phase 0 (previous_window) then phase 2.                  *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Insts, Handles, SurfHandles, Ords, MaxLife, MaxBindings, IdentityReuse, DoubleRelease

NoWin == [h |-> 0, life |-> 0]
NoId == [obj |-> 0, life |-> 0]
NoInst == [st |-> "none", id |-> NoId, w |-> NoWin, surf |-> 0, releases |-> 0]

VARIABLES win, nextLife, inst, cands, req, bs, prov, held, foreignForget

vars == <<win, nextLife, inst, cands, req, bs, prov, held, foreignForget>>

Init ==
  /\ win = [h \in Handles |-> 0]
  /\ nextLife = 1
  /\ inst = [i \in Insts |-> NoInst]
  /\ cands = <<>>
  /\ req = [r \in Ords |-> [prev |-> NoWin, b |-> 0, committed |-> FALSE]]
  /\ bs = <<>>
  /\ prov = [i \in Insts |-> 0]
  /\ held = [i \in Insts |-> 0]
  /\ foreignForget = FALSE

Live(i) == inst[i].st = "live"
CandIdx(id) == {k \in 1..Len(cands) : cands[k].src = id}
Bound(r) == req[r].b # 0
BSrc(r) == bs[req[r].b].src

-----------------------------------------------------------------------------
(* Environment: HWND creation/destruction with WindowLifetime serials.     *)

CreateWindow(h) ==
  /\ win[h] = 0 /\ nextLife <= MaxLife
  /\ win' = [win EXCEPT ![h] = nextLife]
  /\ nextLife' = nextLife + 1
  /\ UNCHANGED <<inst, cands, req, bs, prov, held, foreignForget>>

DestroyWindow(h) ==
  /\ win[h] # 0
  /\ win' = [win EXCEPT ![h] = 0]
  /\ UNCHANGED <<nextLife, inst, cands, req, bs, prov, held, foreignForget>>

-----------------------------------------------------------------------------
(* AssignmentRegistry::register on swapchain / surface creation.           *)
Register(i) ==
  /\ inst[i].st = "none"
  /\ \E h \in Handles, sh \in SurfHandles :
       /\ win[h] # 0
       /\ \A j \in Insts : inst[j].surf # sh
       /\ LET w == [h |-> h, life |-> win[h]]
              id == IF IdentityReuse THEN [obj |-> sh, life |-> win[h]]
                    ELSE [obj |-> i, life |-> i]
          IN IF CandIdx(id) # {}
               THEN /\ inst' = [inst EXCEPT ![i] = [NoInst EXCEPT !.st = "rejected", !.id = id,
                                                    !.w = w, !.surf = sh]]
                    /\ UNCHANGED cands
               ELSE /\ inst' = [inst EXCEPT ![i] = [NoInst EXCEPT !.st = "live", !.id = id,
                                                    !.w = w, !.surf = sh]]
                    /\ cands' = Append(cands, [src |-> id, w |-> w])
  /\ UNCHANGED <<win, nextLife, req, bs, prov, held, foreignForget>>

(* Vulkan Surface::binding: identify the window, then observe().           *)
Observe(i) ==
  /\ Live(i) /\ win[inst[i].w.h] = inst[i].w.life
  /\ LET c == [src |-> inst[i].id, w |-> inst[i].w] IN
     cands' = IF CandIdx(inst[i].id) # {}
                THEN [k \in 1..Len(cands) |-> IF cands[k].src = inst[i].id THEN c ELSE cands[k]]
                ELSE Append(cands, c)
  /\ UNCHANGED <<win, nextLife, inst, req, bs, prov, held, foreignForget>>

(* Assignments::resolve.                                                   *)
Eligible(r, c, phase) == IF phase = 0 THEN req[r].prev = c.w ELSE TRUE

RECURSIVE ResolveOrds(_, _, _)
ResolveOrds(assigned, phase, rs) ==
  IF rs = <<>> THEN assigned
  ELSE LET r == Head(rs)
           ks == {k \in 1..Len(cands) :
                    /\ \A q \in Ords : assigned[q] # cands[k].src
                    /\ Eligible(r, cands[k], phase)}
           next == IF assigned[r] # NoId \/ ks = {} THEN assigned
                   ELSE [assigned EXCEPT ![r] = cands[CHOOSE k \in ks : \A m \in ks : k <= m].src]
       IN ResolveOrds(next, phase, Tail(rs))

OrdSeq == [k \in 1..Cardinality(Ords) |-> k]

Resolve ==
  LET initial == [r \in Ords |-> IF Bound(r) THEN BSrc(r) ELSE NoId]
  IN ResolveOrds(ResolveOrds(initial, 0, OrdSeq), 2, OrdSeq)

(* SourceRegistration::reserve -> AssignmentRegistry::reserve.             *)
Reserve(i) ==
  /\ Live(i) /\ prov[i] = 0 /\ held[i] = 0 /\ Len(bs) < MaxBindings
  /\ LET id == inst[i].id
         a == Resolve
         rs == {r \in Ords : a[r] = id}
     IN /\ \A r \in Ords : ~(Bound(r) /\ BSrc(r) = id)
        /\ CandIdx(id) # {}
        /\ rs # {}
        /\ LET r == CHOOSE x \in rs : \A y \in rs : x <= y
               c == cands[CHOOSE k \in CandIdx(id) : TRUE]
               b == Len(bs) + 1
           IN /\ bs' = Append(bs, [ord |-> r, src |-> id, w |-> c.w, inst |-> i])
              /\ req' = [req EXCEPT ![r].b = b]
              /\ prov' = [prov EXCEPT ![i] = b]
  /\ UNCHANGED <<win, nextLife, inst, cands, held, foreignForget>>

(* ProvisionalBinding::commit (Arc::ptr_eq check).                         *)
Commit(i) ==
  /\ prov[i] # 0
  /\ LET p == prov[i]
         r == bs[p].ord
     IN IF req[r].b = p
          THEN /\ req' = [req EXCEPT ![r].committed = TRUE]
               /\ held' = [held EXCEPT ![i] = p]
          ELSE UNCHANGED <<req, held>>
  /\ prov' = [prov EXCEPT ![i] = 0]
  /\ UNCHANGED <<win, nextLife, inst, cands, bs, foreignForget>>

(* Drop for an uncommitted ProvisionalBinding.                              *)
DropProvisional(i) ==
  /\ prov[i] # 0
  /\ LET p == prov[i]
         r == bs[p].ord
     IN req' = IF req[r].b = p THEN [req EXCEPT ![r].b = 0] ELSE req
  /\ prov' = [prov EXCEPT ![i] = 0]
  /\ UNCHANGED <<win, nextLife, inst, cands, bs, held, foreignForget>>

(* SourceRegistration::next_generation, first critical section; the       *)
(* following reserve() is a separate Reserve step.                         *)
NextGeneration(i) ==
  /\ Live(i) /\ prov[i] = 0 /\ held[i] # 0
  /\ req' = [r \in Ords |->
       IF Bound(r) /\ BSrc(r) = inst[i].id
         THEN [prev |-> bs[req[r].b].w, b |-> 0, committed |-> FALSE]
         ELSE req[r]]
  /\ held' = [held EXCEPT ![i] = 0]
  /\ UNCHANGED <<win, nextLife, inst, cands, bs, prov, foreignForget>>

(* AssignmentRegistry::forget.                                             *)
Forget(i) ==
  LET id == inst[i].id IN
  /\ cands' = SelectSeq(cands, LAMBDA c : c.src # id)
  /\ req' = [r \in Ords |->
       IF Bound(r) /\ BSrc(r) = id
         THEN [prev |-> IF req[r].committed THEN bs[req[r].b].w ELSE req[r].prev,
               b |-> 0, committed |-> FALSE]
         ELSE req[r]]
  /\ foreignForget' = (foreignForget \/ \E j \in Insts \ {i} : Live(j) /\ inst[j].id = id)

(* SourceRegistration::release (D3D12 destroyed()) or its Drop.            *)
Release(i) ==
  /\ \/ Live(i)
     \/ DoubleRelease /\ inst[i].st = "released" /\ inst[i].releases = 1
  /\ Forget(i)
  /\ inst' = [inst EXCEPT ![i].st = "released", ![i].releases = @ + 1]
  /\ UNCHANGED <<win, nextLife, bs, prov, held>>

(* vkDestroySurfaceKHR: the handle may be reused before the Surface Arc,   *)
(* and with it the registration, is dropped.                               *)
DestroySurfaceHandle(i) ==
  /\ inst[i].surf # 0
  /\ inst' = [inst EXCEPT ![i].surf = 0]
  /\ UNCHANGED <<win, nextLife, cands, req, bs, prov, held, foreignForget>>

Finished ==
  /\ \A i \in Insts : inst[i].st # "live" /\ prov[i] = 0
  /\ UNCHANGED vars

Next ==
  \/ Finished
  \/ \E h \in Handles : CreateWindow(h) \/ DestroyWindow(h)
  \/ \E i \in Insts : \/ Register(i) \/ Observe(i) \/ Reserve(i) \/ Commit(i)
                      \/ DropProvisional(i) \/ NextGeneration(i) \/ Release(i)
                      \/ DestroySurfaceHandle(i)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Every live request binding belongs to the live source that reserved it *)
(* and names that source's own window identity.                            *)
BindingMatchesOwner ==
  \A r \in Ords : Bound(r) =>
    LET b == bs[req[r].b] IN
    /\ Live(b.inst)
    /\ b.src = inst[b.inst].id
    /\ b.w = inst[b.inst].w

(* A source never holds a committed binding that the registry gave to a  *)
(* different source.                                                       *)
HeldIsOwn ==
  \A i \in Insts : held[i] # 0 => bs[held[i]].inst = i

NoForeignForget == ~foreignForget

(* Availability: a source is never refused because of an identity that  *)
(* is still registered by an older source.                                 *)
NoRejectedSource == \A i \in Insts : inst[i].st # "rejected"

=============================================================================
