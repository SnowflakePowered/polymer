------------------------------ MODULE AclCredits ------------------------------
(***************************************************************************)
(* Shared HCI ACL credits for one connection handle around disconnection.  *)
(*                                                                         *)
(* Rust mapping (src/transport/bredr/hci/host.rs, credits.rs):             *)
(*   acl          ACL state in the controller: "up", "disconnecting"       *)
(*                (HciHost::disconnect got Command Status for Disconnect), *)
(*                "gone" (controller freed the handle)                     *)
(*   dc           Disconnection Complete is queued for dispatch_events     *)
(*   links        HciHost::links contains the handle                       *)
(*   outstanding  AclCredits::outstanding for the handle                   *)
(*   ctrl         controller buffers holding the handle's packets          *)
(*   nocp         completed packets not yet delivered to acl_worker by     *)
(*                Number Of Completed Packets                              *)
(*   queue        packets for the handle queued in acl_worker              *)
(*   budget       sends the link actor still makes; Channel::drop sends    *)
(*                LinkCommand::Close, whose DisconnectionRequest can be    *)
(*                queued after Disconnection Complete                      *)
(*                                                                         *)
(* A controller discards packets for a handle it no longer knows and never *)
(* reports them completed.  Events reach dispatch_events in order, so a    *)
(* Number Of Completed Packets precedes the Disconnection Complete.         *)
(* Other handles are abstracted: a credit leaked here is lost to them.     *)
(*                                                                         *)
(* Fixed = TRUE models the change: HciHost::disconnect no longer forgets   *)
(* credits on Command Status, and acl_worker drops queued packets whose    *)
(* handle is no longer in `links`.                                         *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Fixed, Capacity, Sends

VARIABLES acl, dc, links, outstanding, ctrl, nocp, queue, budget

vars == <<acl, dc, links, outstanding, ctrl, nocp, queue, budget>>

TypeOK ==
  /\ acl \in {"up", "disconnecting", "gone"}
  /\ dc \in BOOLEAN
  /\ links \in BOOLEAN
  /\ outstanding \in 0..Capacity
  /\ ctrl \in 0..Capacity
  /\ nocp \in 0..Capacity
  /\ queue \in 0..Sends
  /\ budget \in 0..Sends

Init ==
  /\ acl = "up"
  /\ dc = FALSE
  /\ links = TRUE
  /\ outstanding = 0
  /\ ctrl = 0
  /\ nocp = 0
  /\ queue = 0
  /\ budget = Sends

Enqueue ==
  /\ budget > 0
  /\ budget' = budget - 1
  /\ queue' = queue + 1
  /\ UNCHANGED <<acl, dc, links, outstanding, ctrl, nocp>>

Dequeue ==
  /\ queue > 0
  /\ outstanding < Capacity
  /\ queue' = queue - 1
  /\ IF Fixed /\ ~links
       THEN UNCHANGED <<outstanding, ctrl>>
       ELSE /\ outstanding' = outstanding + 1
            /\ ctrl' = IF acl = "gone" THEN ctrl ELSE ctrl + 1
  /\ UNCHANGED <<acl, dc, links, nocp, budget>>

ControllerComplete ==
  /\ acl # "gone"
  /\ ctrl > 0
  /\ ctrl' = ctrl - 1
  /\ nocp' = nocp + 1
  /\ UNCHANGED <<acl, dc, links, outstanding, queue, budget>>

DeliverNocp ==
  /\ nocp > 0
  /\ nocp' = nocp - 1
  /\ outstanding' = IF outstanding > 0 THEN outstanding - 1 ELSE 0
  /\ UNCHANGED <<acl, dc, links, ctrl, queue, budget>>

HostDisconnect ==
  /\ acl = "up"
  /\ acl' = "disconnecting"
  /\ outstanding' = IF Fixed THEN outstanding ELSE 0
  /\ UNCHANGED <<dc, links, ctrl, nocp, queue, budget>>

ControllerDisconnect ==
  /\ acl # "gone"
  /\ acl' = "gone"
  /\ ctrl' = 0
  /\ dc' = TRUE
  /\ UNCHANGED <<links, outstanding, nocp, queue, budget>>

DeliverDisconnection ==
  /\ dc
  /\ nocp = 0
  /\ dc' = FALSE
  /\ links' = FALSE
  /\ outstanding' = 0
  /\ UNCHANGED <<acl, ctrl, nocp, queue, budget>>

Done ==
  /\ acl = "gone"
  /\ ~dc
  /\ queue = 0
  /\ budget = 0
  /\ UNCHANGED vars

Next ==
  \/ Enqueue
  \/ Dequeue
  \/ ControllerComplete
  \/ DeliverNocp
  \/ HostDisconnect
  \/ ControllerDisconnect
  \/ DeliverDisconnection
  \/ Done

Spec == Init /\ [][Next]_vars

NoOvercommit == outstanding >= ctrl + nocp

NoLeakAfterDisconnect == (acl = "gone" /\ ~dc) => outstanding = 0
=============================================================================
