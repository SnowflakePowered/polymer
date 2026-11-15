------------------------------ MODULE AutoAssign ------------------------------
(***************************************************************************)
(* Concurrent automatic player-slot assignment in the input hub.           *)
(*                                                                         *)
(* Rust mapping (src/hub/assignment.rs, src/hub/registry.rs):              *)
(*   Devices   devices whose HubInner::connected runs on different         *)
(*             threads: the SDL3 worker, the Bluetooth backend tasks and   *)
(*             the polymer-input-compose thread, each ending in            *)
(*             try_auto_assign                                             *)
(*   slots     HubInner::assignments, PlayerIndex -> DeviceId              *)
(*   pc        per device: "check" = try_auto_assign reading index_of,     *)
(*             devices and lowest_free; "insert" = assign(.., false)       *)
(*             inserting under the `topology` write guard, where a         *)
(*             displaced device gets `to = None` because explicit is       *)
(*             false; "done"                                               *)
(*   chosen    the index lowest_free returned to try_auto_assign           *)
(*                                                                         *)
(* Fixed = TRUE models the change: try_auto_assign holds the `topology`    *)
(* write guard across the check, lowest_free and the insert.               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Devices, Slots, Fixed

None == "none"

VARIABLES slots, pc, chosen

vars == <<slots, pc, chosen>>

TypeOK ==
  /\ slots \in [Slots -> Devices \cup {None}]
  /\ pc \in [Devices -> {"check", "insert", "done"}]
  /\ chosen \in [Devices -> Slots]

Init ==
  /\ slots = [s \in Slots |-> None]
  /\ pc = [d \in Devices |-> "check"]
  /\ chosen = [d \in Devices |-> CHOOSE s \in Slots : TRUE]

Assigned(d) == \E s \in Slots : slots[s] = d

Free == {s \in Slots : slots[s] = None}

LowestFree == CHOOSE s \in Free : \A t \in Free : s <= t

Check(d) ==
  /\ pc[d] = "check"
  /\ IF Assigned(d) \/ Free = {}
       THEN /\ pc' = [pc EXCEPT ![d] = "done"]
            /\ UNCHANGED <<slots, chosen>>
       ELSE IF Fixed
         THEN /\ slots' = [slots EXCEPT ![LowestFree] = d]
              /\ pc' = [pc EXCEPT ![d] = "done"]
              /\ UNCHANGED chosen
         ELSE /\ chosen' = [chosen EXCEPT ![d] = LowestFree]
              /\ pc' = [pc EXCEPT ![d] = "insert"]
              /\ UNCHANGED slots

Insert(d) ==
  /\ pc[d] = "insert"
  /\ slots' = [s \in Slots |->
                 IF s = chosen[d] THEN d
                 ELSE IF slots[s] = d THEN None
                 ELSE slots[s]]
  /\ pc' = [pc EXCEPT ![d] = "done"]
  /\ UNCHANGED chosen

Done ==
  /\ \A d \in Devices : pc[d] = "done"
  /\ UNCHANGED vars

Next == (\E d \in Devices : Check(d) \/ Insert(d)) \/ Done

Spec == Init /\ [][Next]_vars /\ \A d \in Devices : WF_vars(Check(d) \/ Insert(d))

NoStrandedDevice ==
  \A d \in Devices : pc[d] = "done" /\ Free # {} => Assigned(d)

AllAssigned == <>[](\A d \in Devices : Assigned(d))
=============================================================================
