------------------------- MODULE SwapchainPresentation -------------------------
(***************************************************************************)
(* T1: virtual swapchain presentation protocol (producer, consumer,       *)
(* controller), D3D12 fences on both sides.                               *)
(*                                                                         *)
(* Rust mapping, per resource generation g (one SwapchainController and   *)
(* one ProducerSession / IPC connection each):                            *)
(*   phase[g]       "live" = SwapchainController exists;                   *)
(*                  "retired"/"safe"/"quarantined" = BackendRetirement     *)
(*                  (retirement.rs) and its last RetirementProgress        *)
(*   retiring[g]    SwapchainController::retiring (controller.rs)          *)
(*   token, serial, sub, done, pub                                          *)
(*                  SlotState::{token, serial, submission,                 *)
(*                  producer_complete, publication} (state.rs)             *)
(*   nextValue, nextLease, crel, uncertain                                  *)
(*                  SwapchainController::{next_value, next_lease,          *)
(*                  consumer_release, uncertain}                           *)
(*   work[g]        RetirementWork {acquired, submissions, publications,   *)
(*                  uncertain} built by retirement_work()                  *)
(*   cleanupNext    D3d12VirtualBackend::cleanup_value                     *)
(*                  (polymer-producer-d3d12/src/swapchain/mod.rs)          *)
(*   pq             producer CommandQueue: queue.Signal(ready|cleanup)     *)
(*   ready, cleanup completed values of the ready / cleanup ID3D12Fence    *)
(*   devLost        GetDeviceRemovedReason() is an error                   *)
(*   conn[g]        ProducerSession::connected() / capture socket          *)
(*   imported[g]    D3d12Compositor::surfaces contains the stream          *)
(*                  (renderer-d3d12 compositor/streams.rs)                 *)
(*   retention[g]   ImportedSurface::retention() (fixed at import)         *)
(*   frames[g]      SurfaceFrames {received, pending, displayed, released} *)
(*                  (polymer-renderer/src/renderer/frame_ownership.rs)     *)
(*   relFence[g]    consumer release fence (ImportedD3d12Stream::consumer, *)
(*                  read by the producer as BackendProgress::released)     *)
(*   comp           one composite in flight on the consumer queue:         *)
(*                  slots it reads and the signal_release values after it  *)
(*                  (compositor/present.rs)                                *)
(*   consumerUp     consumer process alive                                 *)
(*   app            D3d12Frontend / State: the lease it holds, the ring    *)
(*                  index it acquires next, and the arm of                 *)
(*                  wait_for_progress (arm_progress)                       *)
(*   closed, oldLease  Vulkan layer: Swapchain::closed and a lease held on *)
(*                  a replaced swapchain (present_retired path)            *)
(*   lastSubmit     ghost: controller and lease generation of the last     *)
(*                  successful take_token                                  *)
(*   acqDone[g]     Vulkan: WriteToken::acquire (the acquire-queue         *)
(*                  ownership barrier) has completed.  One in-order pq     *)
(*                  abstracts the acquire queue, the present queues and    *)
(*                  their private CompletionTimelines.                     *)
(*   killed         hosted launch: the viewer owned the kill-on-close job  *)
(*                  (polymer-platform-windows guest/target.rs), so its     *)
(*                  death ended the producer process too                   *)
(*   vDevLost       viewer device removed: compositor queue signals,      *)
(*                  including signal_release(RELEASE_ALL), never execute   *)
(*                                                                         *)
(* VulkanPath selects polymer-hook-vulkan-layer: Swapchain::acquire,       *)
(* Swapchain::retire and VulkanVirtualBackend::retirement replace the      *)
(* D3d12Frontend and DxgiMaintenance actions.                              *)
(*                                                                         *)
(* Values are FrameReady::ready_value(seq, slot) = 4*seq + slot; u64       *)
(* overflow is abstracted as seq > MaxSeq.  RELEASE_ALL_SYNC_VALUE is a    *)
(* value above every ready value.                                          *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS N, MaxGen, MaxSeq, MaxLease, Latency, MaxFaults,
          CrashFault, DropFault, DeviceFault, IndeterminateFault, ViewerDeviceFault,
          VulkanPath, HostedLaunch

ASSUME N \in 2..4 /\ MaxGen \in Nat \ {0} /\ Latency \in Nat

Gens == 1..MaxGen
Slots == 0..(N - 1)
Val(q, s) == 4 * q + s
SlotOf(v) == v % 4
SeqOf(v) == v \div 4
ReleaseAll == Val(MaxSeq + 2, 0)
Max(a, b) == IF a > b THEN a ELSE b

NoSub == [f |-> "none", v |-> 0]
NoLease == [g |-> 0, s |-> 0, serial |-> 0]
NoArm == [rel |-> 0, sub |-> NoSub]
NoFrames == [recv |-> 0, pend |-> 0, disp |-> 0, rel |-> 0]
NoWork == [acq |-> {}, subs |-> {}, pubs |-> {}, unc |-> FALSE]
Idle == [busy |-> FALSE, reads |-> {}, rels |-> [g \in Gens |-> 0]]

VARIABLES phase, retiring, closed, token, serial, sub, done, pub,
          nextValue, nextLease, crel, uncertain, work, cleanupNext,
          pq, ready, cleanup, devLost,
          conn, imported, retention, frames, relFence, comp, consumerUp,
          app, oldLease, faults, lastSubmit, acqDone, killed, vDevLost

ctlVars == <<phase, retiring, closed, token, serial, sub, done, pub,
             nextValue, nextLease, crel, uncertain, work, cleanupNext>>
gpuVars == <<pq, ready, cleanup, devLost, acqDone>>
conVars == <<conn, imported, retention, frames, relFence, comp, consumerUp, vDevLost>>
appVars == <<app, oldLease, faults, lastSubmit, killed>>
vars == <<ctlVars, gpuVars, conVars, appVars>>

Fence(g, f) == IF f = "ready" THEN ready[g] ELSE cleanup[g]

TypeOK ==
  /\ phase \in [Gens -> {"none", "live", "retired", "safe", "quarantined"}]
  /\ retiring \in [Gens -> {"none", "conn", "dev", "uncertain", "counter"}]
  /\ token \in [Gens -> [Slots -> BOOLEAN]]
  /\ done \in [Gens -> [Slots -> BOOLEAN]]
  /\ conn \in [Gens -> {"none", "up", "down"}]
  /\ app.pc \in {"acquire", "wait", "hold", "failed", "returned", "create", "done"}
  /\ acqDone \in [Gens -> [Slots -> BOOLEAN]]

-----------------------------------------------------------------------------
(* Controller helpers (controller.rs).                                     *)

PollErr(g) == retiring[g] # "none" \/ conn[g] = "down" \/ devLost
PollReason(g) ==
  IF retiring[g] # "none" THEN retiring[g]
  ELSE IF conn[g] = "down" THEN "conn" ELSE "dev"
PolledCrel(g) == Max(crel[g], relFence[g])
PolledDone(g) ==
  [s \in Slots |-> done[g][s] \/ (sub[g][s] # NoSub /\ Fence(g, sub[g][s].f) >= sub[g][s].v)]

Unreleased(g, c) == {pub[g][s] : s \in {x \in Slots : pub[g][x] > c}}
PacingReady(g, c) == Latency # 0 /\ Cardinality(Unreleased(g, c)) <= Latency
PendingRelease(g, c) ==
  IF Unreleased(g, c) = {} THEN 0
  ELSE CHOOSE v \in Unreleased(g, c) : \A w \in Unreleased(g, c) : v <= w
PacingTarget(g, c) ==
  LET P == Unreleased(g, c)
      excess == Cardinality(P) - Latency
  IN IF excess < 1 THEN 0
     ELSE CHOOSE v \in P : Cardinality({w \in P : w <= v}) = excess
OldestInFlight(g, d) ==
  LET S == {s \in Slots : ~token[g][s] /\ ~d[s] /\ sub[g][s] # NoSub}
  IN IF S = {} THEN NoSub
     ELSE sub[g][CHOOSE s \in S : \A t \in S : serial[g][s] <= serial[g][t]]

TakeTokenOK(g, l) == l.g = g /\ token[g][l.s] /\ serial[g][l.s] = l.serial
SubmitExhausted(g) == SeqOf(nextValue[g]) + 1 > MaxSeq

CanFault == faults < MaxFaults

(* submit(): counter check, take_token, reserve next_value, submit_present. *)
Submit(g, l, published, OkStep, ErrStep) ==
  \/ /\ SubmitExhausted(g)
     /\ retiring' = [retiring EXCEPT ![g] = "counter"]
     /\ ErrStep
     /\ UNCHANGED <<phase, closed, token, serial, sub, done, pub, nextValue, nextLease,
                    crel, uncertain, work, cleanupNext, gpuVars, conVars, faults, lastSubmit,
                    killed>>
  \/ /\ ~SubmitExhausted(g) /\ ~TakeTokenOK(g, l)
     /\ ErrStep
     /\ UNCHANGED <<ctlVars, gpuVars, conVars, faults, lastSubmit, killed>>
  \/ /\ ~SubmitExhausted(g) /\ TakeTokenOK(g, l)
     /\ LET v == Val(SeqOf(nextValue[g]) + 1, l.s)
            signal == [f |-> "ready", g |-> g, v |-> v]
        IN /\ nextValue' = [nextValue EXCEPT ![g] = v]
           /\ token' = [token EXCEPT ![g][l.s] = FALSE]
           /\ lastSubmit' = [g |-> g, lg |-> l.g]
           /\ \/ /\ pq' = Append(pq, signal)
                 /\ sub' = [sub EXCEPT ![g][l.s] = [f |-> "ready", v |-> v]]
                 /\ done' = [done EXCEPT ![g][l.s] = FALSE]
                 /\ pub' = IF published THEN [pub EXCEPT ![g][l.s] = v] ELSE pub
                 /\ OkStep
                 /\ UNCHANGED <<retiring, uncertain, faults>>
              \/ /\ IndeterminateFault /\ CanFault
                 /\ faults' = faults + 1
                 /\ uncertain' = [uncertain EXCEPT ![g] = TRUE]
                 /\ retiring' = [retiring EXCEPT ![g] = "uncertain"]
                 /\ \E signalled \in BOOLEAN : pq' = IF signalled THEN Append(pq, signal) ELSE pq
                 /\ ErrStep
                 /\ UNCHANGED <<sub, done, pub>>
     /\ UNCHANGED <<phase, closed, serial, nextLease, crel, work, cleanupNext,
                    ready, cleanup, devLost, acqDone, conVars, killed>>

(* retire(): retirement_work() then SwapchainBackend::retire; dropping the *)
(* controller drops its ProducerSession, which shuts the socket down.      *)
RetireWith(g, tok, sb) ==
  /\ phase' = [phase EXCEPT ![g] = "retired"]
  /\ work' = [work EXCEPT ![g] =
       [acq |-> {s \in Slots : tok[s]},
        subs |-> {sb[s] : s \in {x \in Slots : sb[x] # NoSub}},
        pubs |-> {pub[g][s] : s \in {x \in Slots : pub[g][x] # 0}},
        unc |-> uncertain[g]]]
  /\ token' = [token EXCEPT ![g] = [s \in Slots |-> FALSE]]
  /\ sub' = [sub EXCEPT ![g] = [s \in Slots |-> NoSub]]
  /\ conn' = [conn EXCEPT ![g] = IF @ = "up" THEN "down" ELSE @]

-----------------------------------------------------------------------------
Init ==
  /\ phase = [g \in Gens |-> IF g = 1 THEN "live" ELSE "none"]
  /\ retiring = [g \in Gens |-> "none"]
  /\ closed = [g \in Gens |-> FALSE]
  /\ token = [g \in Gens |-> [s \in Slots |-> FALSE]]
  /\ serial = [g \in Gens |-> [s \in Slots |-> 0]]
  /\ sub = [g \in Gens |-> [s \in Slots |-> NoSub]]
  /\ done = [g \in Gens |-> [s \in Slots |-> TRUE]]
  /\ pub = [g \in Gens |-> [s \in Slots |-> 0]]
  /\ nextValue = [g \in Gens |-> 0]
  /\ nextLease = [g \in Gens |-> 0]
  /\ crel = [g \in Gens |-> 0]
  /\ uncertain = [g \in Gens |-> FALSE]
  /\ work = [g \in Gens |-> NoWork]
  /\ cleanupNext = [g \in Gens |-> 0]
  /\ pq = <<>>
  /\ ready = [g \in Gens |-> 0]
  /\ cleanup = [g \in Gens |-> 0]
  /\ devLost = FALSE
  /\ conn = [g \in Gens |-> IF g = 1 THEN "up" ELSE "none"]
  /\ imported = [g \in Gens |-> g = 1]
  /\ retention \in [Gens -> {"retained", "cached"}]
  /\ frames = [g \in Gens |-> NoFrames]
  /\ relFence = [g \in Gens |-> 0]
  /\ comp = Idle
  /\ consumerUp = TRUE
  /\ app = [pc |-> "acquire", g |-> 1, k |-> 0, paced |-> FALSE, lease |-> NoLease, arm |-> NoArm]
  /\ oldLease = NoLease
  /\ faults = 0
  /\ lastSubmit = [g |-> 0, lg |-> 0]
  /\ acqDone = [g \in Gens |-> [s \in Slots |-> TRUE]]
  /\ killed = FALSE
  /\ vDevLost = FALSE

-----------------------------------------------------------------------------
(* Application / frontend on the API owner thread.                         *)

(* One iteration of acquire_with_info: poll(), pacing_ready, try_acquire / *)
(* acquire_slot, else wait_for_progress arms the release target and the    *)
(* oldest in-flight submission.                                            *)
AcquireWithInfo ==
  LET g == app.g IN
  /\ app.pc = "acquire" /\ phase[g] = "live"
  /\ IF PollErr(g)
       THEN /\ retiring' = [retiring EXCEPT ![g] = PollReason(g)]
            /\ app' = [app EXCEPT !.pc = "failed"]
            /\ UNCHANGED <<phase, closed, token, serial, sub, done, pub, nextValue, nextLease,
                           crel, uncertain, work, cleanupNext>>
       ELSE LET c == PolledCrel(g)
                d == PolledDone(g)
                k == app.k
                paced == Latency = 0 \/ ~app.paced \/ PacingReady(g, c)
                free == ~token[g][k] /\ d[k] /\ pub[g][k] <= c
            IN /\ crel' = [crel EXCEPT ![g] = c]
               /\ done' = [done EXCEPT ![g] = d]
               /\ IF paced /\ free
                    THEN IF nextLease[g] >= MaxLease
                           THEN /\ retiring' = [retiring EXCEPT ![g] = "counter"]
                                /\ app' = [app EXCEPT !.pc = "failed"]
                                /\ UNCHANGED <<token, serial, sub, pub, nextLease>>
                           ELSE LET sr == nextLease[g] + 1 IN
                                /\ token' = [token EXCEPT ![g][k] = TRUE]
                                /\ serial' = [serial EXCEPT ![g][k] = sr]
                                /\ sub' = [sub EXCEPT ![g][k] = NoSub]
                                /\ pub' = [pub EXCEPT ![g][k] = 0]
                                /\ nextLease' = [nextLease EXCEPT ![g] = sr]
                                /\ app' = [app EXCEPT !.pc = "hold",
                                                      !.lease = [g |-> g, s |-> k, serial |-> sr]]
                                /\ IF VulkanPath
                                     THEN /\ pq' = Append(pq, [f |-> "acquire", g |-> g, v |-> k])
                                          /\ acqDone' = [acqDone EXCEPT ![g][k] = FALSE]
                                     ELSE UNCHANGED <<pq, acqDone>>
                                /\ UNCHANGED retiring
                    ELSE /\ app' = [app EXCEPT !.pc = "wait", !.arm =
                              [rel |-> IF Latency # 0 /\ app.paced /\ ~paced
                                         THEN PacingTarget(g, c) ELSE PendingRelease(g, c),
                               sub |-> OldestInFlight(g, d)]]
                         /\ UNCHANGED <<retiring, token, serial, sub, pub, nextLease, pq, acqDone>>
               /\ UNCHANGED <<phase, closed, nextValue, uncertain, work, cleanupNext>>
  /\ (PollErr(g) \/ ~(app'.pc = "hold")) => UNCHANGED <<pq, acqDone>>
  /\ UNCHANGED <<ready, cleanup, devLost, conVars, oldLease, faults, lastSubmit, killed>>

(* ProgressEvent wakes on the armed release value or submission; with     *)
(* nothing armed, arm_progress returns false and deadline.pause() retries. *)
WaitForProgress ==
  LET a == app.arm
      g == app.g
  IN /\ app.pc = "wait"
     /\ \/ a.rel = 0 /\ a.sub = NoSub
        \/ a.rel # 0 /\ relFence[g] >= a.rel
        \/ a.sub # NoSub /\ Fence(g, a.sub.f) >= a.sub.v
        \/ devLost
     /\ app' = [app EXCEPT !.pc = "acquire", !.arm = NoArm]
     /\ UNCHANGED <<ctlVars, gpuVars, conVars, oldLease, faults, lastSubmit, killed>>

(* AcquireDeadline expired: ControllerError::Timeout; the frontend fails.  *)
(* Vulkan: Swapchain::acquire returns VK_TIMEOUT / VK_NOT_READY to the     *)
(* application, which may acquire again or destroy the swapchain.          *)
Timeout ==
  /\ app.pc = "wait"
  /\ app' = [app EXCEPT !.pc = IF VulkanPath THEN "returned" ELSE "failed", !.arm = NoArm]
  /\ UNCHANGED <<ctlVars, gpuVars, conVars, oldLease, faults, lastSubmit, killed>>

RetryAcquire ==
  /\ VulkanPath /\ app.pc = "returned"
  /\ app' = [app EXCEPT !.pc = "acquire"]
  /\ UNCHANGED <<ctlVars, gpuVars, conVars, oldLease, faults, lastSubmit, killed>>

(* present(): available() then submit(published = publisher.is_some()).    *)
Present ==
  LET g == app.g
      failed == [app EXCEPT !.pc = "failed", !.lease = NoLease]
  IN /\ app.pc = "hold" /\ phase[g] = "live"
     /\ IF retiring[g] # "none" \/ conn[g] = "down"
          THEN /\ retiring' = [retiring EXCEPT ![g] = IF @ = "none" THEN "conn" ELSE @]
               /\ app' = failed
               /\ UNCHANGED <<phase, closed, token, serial, sub, done, pub, nextValue, nextLease,
                              crel, uncertain, work, cleanupNext, gpuVars, conVars, faults, lastSubmit, killed>>
          ELSE Submit(g, app.lease, TRUE,
                      app' = [app EXCEPT !.pc = "acquire", !.k = (app.k + 1) % N,
                                         !.paced = TRUE, !.lease = NoLease],
                      app' = failed)
     /\ UNCHANGED oldLease

(* D3d12Frontend::take_retirement: cancel held leases, then retire().      *)
TakeRetirement ==
  LET g == app.g
      l == app.lease
      c == cleanupNext[g] + 1
      holding == app.pc = "hold"
  IN /\ ~VulkanPath
     /\ app.pc \in {"hold", "failed"} /\ phase[g] = "live"
     /\ IF holding
          THEN /\ pq' = Append(pq, [f |-> "cleanup", g |-> g, v |-> c])
               /\ cleanupNext' = [cleanupNext EXCEPT ![g] = c]
               /\ done' = [done EXCEPT ![g][l.s] = FALSE]
               /\ RetireWith(g, [token[g] EXCEPT ![l.s] = FALSE],
                             [sub[g] EXCEPT ![l.s] = [f |-> "cleanup", v |-> c]])
          ELSE /\ RetireWith(g, token[g], sub[g])
               /\ UNCHANGED <<pq, cleanupNext, done>>
     /\ app' = [app EXCEPT !.pc = "create", !.lease = NoLease]
     /\ UNCHANGED <<retiring, closed, serial, pub, nextValue, nextLease, crel, uncertain,
                    ready, cleanup, devLost, acqDone, imported, retention, frames, relFence,
                    comp, consumerUp, vDevLost, oldLease, faults, lastSubmit, killed>>

(* SwapchainController::create for the next generation; publish() needs   *)
(* a live viewer, otherwise the hook falls back to native presentation.    *)
Create ==
  LET g == app.g + 1 IN
  /\ app.pc = "create"
  /\ IF g > MaxGen \/ ~consumerUp
       THEN /\ app' = [app EXCEPT !.pc = "done"]
            /\ UNCHANGED <<phase, conn, imported>>
       ELSE /\ phase' = [phase EXCEPT ![g] = "live"]
            /\ conn' = [conn EXCEPT ![g] = "up"]
            /\ imported' = [imported EXCEPT ![g] = TRUE]
            /\ app' = [pc |-> "acquire", g |-> g, k |-> 0, paced |-> FALSE,
                       lease |-> NoLease, arm |-> NoArm]
  /\ UNCHANGED <<retiring, closed, token, serial, sub, done, pub, nextValue, nextLease, crel,
                 uncertain, work, cleanupNext, gpuVars, retention, frames, relFence, comp,
                 consumerUp, vDevLost, oldLease, faults, lastSubmit, killed>>

(* Vulkan layer Swapchain::create with oldSwapchain: the old generation is *)
(* closed while the application still owns an image of it.                 *)
CloseOldSwapchain ==
  /\ VulkanPath
  /\ app.pc = "hold" /\ app.g < MaxGen /\ oldLease = NoLease
  /\ closed' = [closed EXCEPT ![app.g] = TRUE]
  /\ oldLease' = app.lease
  /\ app' = [app EXCEPT !.pc = "create", !.lease = NoLease]
  /\ UNCHANGED <<phase, retiring, token, serial, sub, done, pub, nextValue, nextLease, crel,
                 uncertain, work, cleanupNext, gpuVars, conVars, faults, lastSubmit, killed>>

(* queue_present on a closed swapchain: present_retired -> submit(false).  *)
PresentRetired ==
  LET g == oldLease.g IN
  /\ g # 0 /\ phase[g] = "live"
  /\ Submit(g, oldLease, FALSE, oldLease' = NoLease, oldLease' = NoLease)
  /\ UNCHANGED app

(* Swapchain::retire on the closed generation: controller.retire() without *)
(* cancelling; outstanding tokens move into RetirementWork::acquired.      *)
RetireClosed(g) ==
  /\ ~killed /\ closed[g] /\ phase[g] = "live"
  /\ RetireWith(g, token[g], sub[g])
  /\ oldLease' = IF oldLease.g = g THEN NoLease ELSE oldLease
  /\ UNCHANGED <<retiring, closed, serial, done, pub, nextValue, nextLease, crel, uncertain,
                 cleanupNext, gpuVars, imported, retention, frames, relFence, comp, consumerUp,
                 vDevLost, app, faults, lastSubmit, killed>>

(* vkDestroySwapchainKHR on the current swapchain -> Swapchain::retire:    *)
(* controller.retire(ApplicationDestroyed) without cancelling held leases. *)
DestroyCurrent ==
  LET g == app.g IN
  /\ VulkanPath /\ app.pc \in {"hold", "failed", "returned"} /\ phase[g] = "live"
  /\ closed' = [closed EXCEPT ![g] = TRUE]
  /\ RetireWith(g, token[g], sub[g])
  /\ app' = [app EXCEPT !.pc = "create", !.lease = NoLease, !.arm = NoArm]
  /\ UNCHANGED <<retiring, serial, done, pub, nextValue, nextLease, crel, uncertain,
                 cleanupNext, gpuVars, imported, retention, frames, relFence, comp, consumerUp,
                 vDevLost, oldLease, faults, lastSubmit, killed>>

(* A lease of one controller handed to another controller's present().    *)
StalePresent ==
  LET g == app.g IN
  /\ oldLease # NoLease /\ oldLease.g # g /\ phase[g] = "live"
  /\ app.pc \in {"hold", "acquire"}
  /\ IF retiring[g] # "none" \/ conn[g] = "down"
       THEN /\ retiring' = [retiring EXCEPT ![g] = IF @ = "none" THEN "conn" ELSE @]
            /\ oldLease' = NoLease
            /\ UNCHANGED <<phase, closed, token, serial, sub, done, pub, nextValue, nextLease,
                           crel, uncertain, work, cleanupNext, gpuVars, conVars, faults, lastSubmit, killed>>
       ELSE Submit(g, oldLease, TRUE, oldLease' = NoLease, oldLease' = NoLease)
  /\ UNCHANGED app

-----------------------------------------------------------------------------
(* VulkanVirtualBackend::retirement: no cleanup submission; acquired      *)
(* tokens wait for their acquire barrier.  Publications are not awaited:   *)
(* the viewer's OpenSharedHandle reference keeps each NT-handle resource   *)
(* alive after the producer drops its own.                                 *)
VulkanRetirement(g) ==
  LET w == work[g] IN
  /\ phase[g] = "retired"
  /\ IF w.unc \/ devLost
       THEN phase' = [phase EXCEPT ![g] = "quarantined"]
       ELSE /\ \A s \in w.acq : acqDone[g][s]
            /\ \A sb \in w.subs : Fence(g, sb.f) >= sb.v
            /\ phase' = [phase EXCEPT ![g] = "safe"]
  /\ UNCHANGED <<retiring, closed, token, serial, sub, done, pub, nextValue, nextLease, crel,
                 uncertain, work, cleanupNext, gpuVars, conVars, appVars>>

(* DxgiMaintenance polling BackendRetirement::poll ->                      *)
(* D3d12VirtualBackend::poll_retirement.                                   *)
DxgiRetirement(g) ==
  /\ phase[g] = "retired"
  /\ LET w == work[g] IN
     IF w.unc \/ devLost
       THEN /\ phase' = [phase EXCEPT ![g] = "quarantined"]
            /\ UNCHANGED <<work, cleanupNext, pq>>
       ELSE IF w.acq # {}
         THEN \E s \in w.acq :
                LET c == cleanupNext[g] + 1 IN
                /\ cleanupNext' = [cleanupNext EXCEPT ![g] = c]
                /\ pq' = Append(pq, [f |-> "cleanup", g |-> g, v |-> c])
                /\ work' = [work EXCEPT ![g].acq = @ \ {s},
                                        ![g].subs = @ \cup {[f |-> "cleanup", v |-> c]}]
                /\ UNCHANGED phase
         ELSE /\ \A sb \in w.subs : Fence(g, sb.f) >= sb.v
              /\ \A p \in w.pubs : relFence[g] >= p
              /\ phase' = [phase EXCEPT ![g] = "safe"]
              /\ UNCHANGED <<work, cleanupNext, pq>>
  /\ UNCHANGED <<retiring, closed, token, serial, sub, done, pub, nextValue, nextLease, crel,
                 uncertain, ready, cleanup, devLost, acqDone, conVars, appVars>>

(* Vulkan Device::poll_retired runs only inside Swapchain::acquire (every  *)
(* loop iteration); DxgiMaintenance polls on its own thread.               *)
PollRetirement(g) ==
  /\ ~killed
  /\ IF VulkanPath
       THEN app.pc \in {"acquire", "wait"} /\ VulkanRetirement(g)
       ELSE DxgiRetirement(g)

(* Swapchain::retire, vkDeviceWaitIdle and the 2s Device::destroy loop:   *)
(* polls that nothing obliges to recur.                                    *)
IncidentalPoll(g) ==
  /\ VulkanPath /\ ~killed /\ app.pc \in {"create", "returned", "failed", "done"}
  /\ VulkanRetirement(g)

-----------------------------------------------------------------------------
(* Producer GPU: the command queue completes signals in order.             *)
ProducerGpu ==
  /\ pq # <<>> /\ ~devLost
  /\ LET h == Head(pq) IN
     /\ ready' = IF h.f = "ready" THEN [ready EXCEPT ![h.g] = h.v] ELSE ready
     /\ cleanup' = IF h.f = "cleanup" THEN [cleanup EXCEPT ![h.g] = h.v] ELSE cleanup
     /\ acqDone' = IF h.f = "acquire" THEN [acqDone EXCEPT ![h.g][h.v] = TRUE] ELSE acqDone
  /\ pq' = Tail(pq)
  /\ UNCHANGED <<ctlVars, devLost, conVars, appVars>>

-----------------------------------------------------------------------------
(* Consumer.                                                               *)

(* CaptureShared::watch_frames: ProducerTimeline::wait_beyond returns the *)
(* newest completed ready value; set_latest -> SurfaceFrames::offer.       *)
WatchFrames(g) ==
  /\ consumerUp /\ imported[g]
  /\ ready[g] > frames[g].recv
  /\ frames' = [frames EXCEPT ![g].recv = ready[g], ![g].pend = ready[g]]
  /\ UNCHANGED <<ctlVars, gpuVars, conn, imported, retention, relFence, comp, consumerUp,
                 vDevLost, appVars>>

ReleaseValue(g) ==
  LET fr == frames[g]
      v == fr.pend - (IF retention[g] = "retained" THEN 1 ELSE 0)
  IN IF fr.pend # 0 /\ v > fr.rel THEN v ELSE 0

Reads(g) ==
  LET fr == frames[g] IN
  IF fr.pend # 0 THEN {<<g, SlotOf(fr.pend)>>}
  ELSE IF retention[g] = "retained" /\ fr.disp # 0 THEN {<<g, SlotOf(fr.disp)>>}
  ELSE {}

(* D3d12Compositor present: record reads, ExecuteCommandLists, queue       *)
(* signal_release(release_value), then SurfaceFrames::commit.              *)
Composite ==
  /\ consumerUp /\ ~vDevLost /\ ~comp.busy
  /\ \E g \in Gens : imported[g] /\ frames[g].pend # 0
  /\ comp' = [busy |-> TRUE,
              reads |-> UNION {Reads(g) : g \in {x \in Gens : imported[x]}},
              rels |-> [g \in Gens |-> IF imported[g] THEN ReleaseValue(g) ELSE 0]]
  /\ frames' = [g \in Gens |->
       IF imported[g] /\ frames[g].pend # 0
         THEN [frames[g] EXCEPT !.rel = IF ReleaseValue(g) # 0 THEN ReleaseValue(g) ELSE @,
                                !.disp = frames[g].pend, !.pend = 0]
         ELSE frames[g]]
  /\ UNCHANGED <<ctlVars, gpuVars, conn, imported, retention, relFence, consumerUp,
                 vDevLost, appVars>>

ConsumerGpu ==
  /\ consumerUp /\ ~vDevLost /\ comp.busy
  /\ relFence' = [g \in Gens |-> IF comp.rels[g] # 0 THEN comp.rels[g] ELSE relFence[g]]
  /\ comp' = Idle
  /\ UNCHANGED <<ctlVars, gpuVars, conn, imported, retention, frames, consumerUp,
                 vDevLost, appVars>>

(* disconnected callback -> disconnect_stream: remove the surface, signal  *)
(* RELEASE_ALL_SYNC_VALUE and wait_idle.  On a removed viewer device the   *)
(* surface is already gone when the queue signal fails.                    *)
DisconnectStream(g) ==
  /\ consumerUp /\ imported[g] /\ conn[g] = "down" /\ ~comp.busy
  /\ imported' = [imported EXCEPT ![g] = FALSE]
  /\ relFence' = IF vDevLost THEN relFence ELSE [relFence EXCEPT ![g] = ReleaseAll]
  /\ frames' = [frames EXCEPT ![g] = NoFrames]
  /\ UNCHANGED <<ctlVars, gpuVars, conn, retention, comp, consumerUp, vDevLost, appVars>>

-----------------------------------------------------------------------------
(* Faults.                                                                 *)

ConnectionDrop(g) ==
  /\ DropFault /\ CanFault
  /\ conn[g] = "up"
  /\ conn' = [conn EXCEPT ![g] = "down"]
  /\ faults' = faults + 1
  /\ UNCHANGED <<ctlVars, gpuVars, imported, retention, frames, relFence, comp, consumerUp,
                 vDevLost, app, oldLease, lastSubmit, killed>>

(* Viewer process death.  Hosted: the kill-on-close job ends the producer *)
(* process; the OS reclaims its queue, allocations and permits.            *)
ConsumerCrash ==
  /\ CrashFault /\ CanFault /\ consumerUp
  /\ consumerUp' = FALSE
  /\ conn' = [g \in Gens |-> IF conn[g] = "up" THEN "down" ELSE conn[g]]
  /\ imported' = [g \in Gens |-> FALSE]
  /\ comp' = Idle
  /\ faults' = faults + 1
  /\ IF HostedLaunch
       THEN /\ killed' = TRUE
            /\ app' = [app EXCEPT !.pc = "done", !.lease = NoLease, !.arm = NoArm]
            /\ oldLease' = NoLease
            /\ pq' = <<>>
       ELSE UNCHANGED <<killed, app, oldLease, pq>>
  /\ UNCHANGED <<ctlVars, ready, cleanup, devLost, acqDone, retention, frames, relFence,
                 vDevLost, lastSubmit>>

(* A new viewer process accepts new connections; the old generations'     *)
(* release timelines stay where the dead viewer left them.                 *)
ConsumerRestart ==
  /\ ~HostedLaunch /\ ~consumerUp
  /\ consumerUp' = TRUE
  /\ vDevLost' = FALSE
  /\ UNCHANGED <<ctlVars, gpuVars, conn, imported, retention, frames, relFence, comp, appVars>>

(* Viewer device removed while its process lives: no further reads and no *)
(* further release signals.                                                *)
ViewerDeviceLost ==
  /\ ViewerDeviceFault /\ CanFault /\ consumerUp /\ ~vDevLost
  /\ vDevLost' = TRUE
  /\ comp' = Idle
  /\ faults' = faults + 1
  /\ UNCHANGED <<ctlVars, gpuVars, conn, imported, retention, frames, relFence, consumerUp,
                 app, oldLease, lastSubmit, killed>>

DeviceLost ==
  /\ DeviceFault /\ CanFault /\ ~devLost
  /\ devLost' = TRUE
  /\ faults' = faults + 1
  /\ UNCHANGED <<ctlVars, pq, ready, cleanup, acqDone, conVars, app, oldLease, lastSubmit, killed>>

Finished ==
  /\ app.pc = "done"
  /\ UNCHANGED vars

-----------------------------------------------------------------------------
Next ==
  \/ AcquireWithInfo \/ WaitForProgress \/ Timeout \/ RetryAcquire \/ Present
  \/ TakeRetirement \/ DestroyCurrent \/ Create
  \/ CloseOldSwapchain \/ PresentRetired \/ StalePresent
  \/ \E g \in Gens : RetireClosed(g) \/ PollRetirement(g) \/ IncidentalPoll(g)
                     \/ WatchFrames(g) \/ DisconnectStream(g) \/ ConnectionDrop(g)
  \/ ProducerGpu \/ Composite \/ ConsumerGpu
  \/ ConsumerCrash \/ ConsumerRestart \/ ViewerDeviceLost \/ DeviceLost
  \/ Finished

(* Timeout fairness stands for a finite acquire deadline.  Poll fairness  *)
(* is strong: Vulkan polls only while the application keeps acquiring.     *)
Fairness ==
  /\ WF_vars(AcquireWithInfo) /\ WF_vars(WaitForProgress) /\ WF_vars(Timeout)
  /\ WF_vars(RetryAcquire) /\ WF_vars(ConsumerRestart)
  /\ WF_vars(ProducerGpu) /\ WF_vars(Composite) /\ WF_vars(ConsumerGpu)
  /\ \A g \in Gens : WF_vars(WatchFrames(g)) /\ SF_vars(DisconnectStream(g))
                     /\ SF_vars(PollRetirement(g))

Spec == Init /\ [][Next]_vars /\ Fairness

(* An application that keeps rendering and recreating its swapchain.      *)
RenderingSpec ==
  /\ Spec
  /\ WF_vars(Present) /\ WF_vars(PresentRetired) /\ WF_vars(DestroyCurrent) /\ WF_vars(Create)

-----------------------------------------------------------------------------
(* Properties.                                                             *)

ConsumerMayRead(g, s) ==
  /\ consumerUp /\ imported[g]
  /\ \/ ready[g] > frames[g].recv /\ SlotOf(ready[g]) = s
     \/ frames[g].pend # 0 /\ SlotOf(frames[g].pend) = s
     \/ retention[g] = "retained" /\ frames[g].disp # 0 /\ SlotOf(frames[g].disp) = s
     \/ <<g, s>> \in comp.reads

ProducerMayWrite(g, s) ==
  \/ app.lease.g = g /\ app.lease.s = s
  \/ oldLease.g = g /\ oldLease.s = s
  \/ \E i \in 1..Len(pq) : pq[i].f = "ready" /\ pq[i].g = g /\ SlotOf(pq[i].v) = s

NoWriteWhileRead ==
  \A g \in Gens, s \in Slots : ~(ConsumerMayRead(g, s) /\ ProducerMayWrite(g, s))

(* Vulkan relies on the viewer's own allocation reference instead of its  *)
(* release timeline, so only the producer's side is checked there.         *)
SafeRetirement ==
  \A g \in Gens : phase[g] = "safe" =>
    /\ \A s \in Slots : /\ ~ProducerMayWrite(g, s) /\ acqDone[g][s]
                        /\ (VulkanPath \/ ~ConsumerMayRead(g, s))
    /\ \A i \in 1..Len(pq) : pq[i].g # g

SubmitOnlyOwnLease == lastSubmit.g = lastSubmit.lg

ValuesBelowReleaseAll ==
  /\ \A g \in Gens : ready[g] < ReleaseAll /\ nextValue[g] < ReleaseAll
  /\ \A i \in 1..Len(pq) : pq[i].v < ReleaseAll

Monotonic ==
  [][\A g \in Gens : /\ crel'[g] >= crel[g] /\ ready'[g] >= ready[g]
                     /\ relFence'[g] >= relFence[g] /\ nextValue'[g] >= nextValue[g]]_vars

CounterExhaustedStops ==
  [][\A g \in Gens : retiring[g] = "counter" =>
       nextValue'[g] = nextValue[g] /\ nextLease'[g] = nextLease[g]]_vars

AcquireSucceeds ==
  \A g \in Gens : (app.g = g /\ app.pc \in {"acquire", "wait"})
                  ~> (app.pc \in {"hold", "failed", "returned"} \/ app.g # g)

(* A GenerationPermit returns unless the producer itself lost track of its *)
(* GPU work (Quarantined keeps the permit), or the process ended.          *)
ProducerFault(g) == work[g].unc \/ devLost

PermitReturns ==
  \A g \in Gens : phase[g] = "retired"
                  ~> (phase[g] = "safe" \/ ProducerFault(g) \/ killed \/ app.pc = "done")

(* As PermitReturns, without relying on the application to keep running.  *)
PermitReturnsIdle ==
  \A g \in Gens : phase[g] = "retired" ~> (phase[g] = "safe" \/ ProducerFault(g) \/ killed)

=============================================================================
