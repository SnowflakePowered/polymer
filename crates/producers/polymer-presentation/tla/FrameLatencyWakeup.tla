-------------------------- MODULE FrameLatencyWakeup --------------------------
(***************************************************************************)
(* The application-visible frame latency waitable racing the consumer's    *)
(* release fence.                                                          *)
(*                                                                         *)
(* Rust mapping (src/swapchain/latency.rs, FrameLatency::refresh):         *)
(*   fence     the release fence's completed value as read by              *)
(*             SwapchainController::poll through backend poll_progress     *)
(*             (D3D12 `released.fence`, D3D11 `consumer_fence`); 1 is the  *)
(*             pacing target pacing_release_target returns                 *)
(*   event     the manual-reset event behind GetFrameLatencyWaitableObject *)
(*   pending   a SetEventOnCompletion(1, event) registration on that fence *)
(*             is outstanding; it fires once and is consumed               *)
(*   armed     FrameLatency::armed, the last target passed to arm_progress *)
(*   pc        the frontend thread inside refresh, under the controller's  *)
(*             owner guard: "idle", "reset" = signal(false), "check" =     *)
(*             wait_for_pacing(Nonblocking), "arm" = the armed.swap filter *)
(*             and arm_progress, "set" = signal(true)                      *)
(*   calls     refresh invocations left (Present, SetMaximumFrameLatency)  *)
(*                                                                         *)
(* SetEventOnCompletion on a value the fence already reached sets the      *)
(* event at once, so arming is atomic with that check.                     *)
(*                                                                         *)
(* Fixed = TRUE models the change: refresh resets the event before it      *)
(* polls the controller instead of after a WouldBlock result.              *)
(***************************************************************************)
EXTENDS Integers, TLC

CONSTANTS Fixed, Refreshes

VARIABLES fence, event, pending, armed, pc, calls

vars == <<fence, event, pending, armed, pc, calls>>

TypeOK ==
  /\ fence \in 0..1
  /\ event \in BOOLEAN
  /\ pending \in BOOLEAN
  /\ armed \in 0..1
  /\ pc \in {"idle", "reset", "check", "arm", "set"}
  /\ calls \in 0..Refreshes

Init ==
  /\ fence = 0
  /\ event = FALSE
  /\ pending = FALSE
  /\ armed = 0
  /\ pc = "idle"
  /\ calls = Refreshes

Release ==
  /\ fence = 0
  /\ fence' = 1
  /\ event' = (event \/ pending)
  /\ pending' = FALSE
  /\ UNCHANGED <<armed, pc, calls>>

Begin ==
  /\ pc = "idle"
  /\ calls > 0
  /\ pc' = IF Fixed THEN "reset" ELSE "check"
  /\ UNCHANGED <<fence, event, pending, armed, calls>>

Check ==
  /\ pc = "check"
  /\ pc' = CASE fence >= 1 -> "set"
             [] Fixed -> "arm"
             [] OTHER -> "reset"
  /\ UNCHANGED <<fence, event, pending, armed, calls>>

Reset ==
  /\ pc = "reset"
  /\ event' = FALSE
  /\ pc' = IF Fixed THEN "check" ELSE "arm"
  /\ UNCHANGED <<fence, pending, armed, calls>>

Arm ==
  /\ pc = "arm"
  /\ IF armed # 1
       THEN /\ armed' = 1
            /\ IF fence >= 1
                 THEN /\ event' = TRUE
                      /\ UNCHANGED pending
                 ELSE /\ pending' = TRUE
                      /\ UNCHANGED event
       ELSE UNCHANGED <<armed, event, pending>>
  /\ pc' = "idle"
  /\ calls' = calls - 1
  /\ UNCHANGED fence

Set ==
  /\ pc = "set"
  /\ event' = TRUE
  /\ pc' = "idle"
  /\ calls' = calls - 1
  /\ UNCHANGED <<fence, pending, armed>>

Done ==
  /\ pc = "idle"
  /\ calls = 0
  /\ fence = 1
  /\ UNCHANGED vars

Next == Release \/ Begin \/ Check \/ Reset \/ Arm \/ Set \/ Done

Spec == Init /\ [][Next]_vars /\ WF_vars(Release) /\ WF_vars(Check \/ Reset \/ Arm \/ Set)

NoLostWakeup ==
  (pc = "idle" /\ calls < Refreshes /\ fence >= 1) => event
=============================================================================
