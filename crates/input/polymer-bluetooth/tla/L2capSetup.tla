----------------------------- MODULE L2capSetup -----------------------------
(***************************************************************************)
(* L2CAP channel setup on one PSM when a caller gives up and retries.      *)
(*                                                                         *)
(* Rust mapping (src/transport/bredr/l2cap/link.rs):                       *)
(*   caller    the task in Link::request_channel: "idle" (between          *)
(*             attempts, e.g. open_channel_with_retry's sleep),            *)
(*             "waiting" on the oneshot, "open" (received a Channel),      *)
(*             "failed" (last attempt timed out)                           *)
(*   attempt   attempts made so far                                        *)
(*   accepts   link_actor's `accepts` VecDeque for the PSM, in push order; *)
(*             live = FALSE once request_channel's time::timeout dropped   *)
(*             the receiver (oneshot::Sender::is_closed)                   *)
(*   opens     link_actor's `pending` entries for the PSM created by       *)
(*             LinkCommand::Open, the coalescing candidates found through  *)
(*             `connection_ids` (HashMap order = any element)              *)
(*   peer      the peer's own ConnectionRequest for the PSM: "idle",       *)
(*             "sent" (frame in flight), "handled"                         *)
(*   stored    the request was queued in `stored_requests`                 *)
(*   dead      the request was handed to a closed oneshot: the Channel     *)
(*             finish_if_ready builds is dropped, its Drop sends           *)
(*             DisconnectionRequest and the peer's channel is gone         *)
(*                                                                         *)
(* The peer answers none of the host's own ConnectionRequests (the crossed *)
(* Nintendo case, result 1 = pending) and sends its own request once.      *)
(* Configuration after the hand-off is abstracted (L2capConfig.tla).       *)
(*                                                                         *)
(* Fixed = TRUE models discard_abandoned: every command and every          *)
(* signaling frame first drops closed accept waiters and pending entries.  *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANTS Fixed, Attempts

VARIABLES caller, attempt, accepts, opens, peer, stored, dead

vars == <<caller, attempt, accepts, opens, peer, stored, dead>>

Target == [id : 1..Attempts, live : BOOLEAN]

TypeOK ==
  /\ caller \in {"idle", "waiting", "open", "failed"}
  /\ attempt \in 0..Attempts
  /\ accepts \in Seq(Target)
  /\ opens \subseteq Target
  /\ peer \in {"idle", "sent", "handled"}
  /\ stored \in BOOLEAN
  /\ dead \in BOOLEAN

Init ==
  /\ caller = "idle"
  /\ attempt = 0
  /\ accepts = <<>>
  /\ opens = {}
  /\ peer = "idle"
  /\ stored = FALSE
  /\ dead = FALSE

IsLive(t) == t.live

LiveAccepts == IF Fixed THEN SelectSeq(accepts, IsLive) ELSE accepts

LiveOpens == IF Fixed THEN {t \in opens : t.live} ELSE opens

Kill(t) == IF t.id = attempt THEN [t EXCEPT !.live = FALSE] ELSE t

Request(kind) ==
  /\ caller = "idle"
  /\ attempt < Attempts
  /\ attempt' = attempt + 1
  /\ IF stored
       THEN /\ caller' = "open"
            /\ stored' = FALSE
            /\ accepts' = LiveAccepts
            /\ opens' = LiveOpens
       ELSE /\ caller' = "waiting"
            /\ stored' = FALSE
            /\ IF kind = "accept"
                 THEN /\ accepts' = Append(LiveAccepts, [id |-> attempt + 1, live |-> TRUE])
                      /\ opens' = LiveOpens
                 ELSE /\ accepts' = LiveAccepts
                      /\ opens' = LiveOpens \cup {[id |-> attempt + 1, live |-> TRUE]}
  /\ UNCHANGED <<peer, dead>>

Timeout ==
  /\ caller = "waiting"
  /\ accepts' = [i \in 1..Len(accepts) |-> Kill(accepts[i])]
  /\ opens' = {Kill(t) : t \in opens}
  /\ caller' = IF attempt < Attempts THEN "idle" ELSE "failed"
  /\ UNCHANGED <<attempt, peer, stored, dead>>

PeerRequest ==
  /\ peer = "idle"
  /\ peer' = "sent"
  /\ UNCHANGED <<caller, attempt, accepts, opens, stored, dead>>

HandOff(t) ==
  /\ caller' = IF t.live THEN "open" ELSE caller
  /\ dead' = ~t.live

ActorReceive ==
  /\ peer = "sent"
  /\ peer' = "handled"
  /\ attempt' = attempt
  /\ IF LiveAccepts # <<>>
       THEN /\ accepts' = Tail(LiveAccepts)
            /\ opens' = LiveOpens
            /\ stored' = stored
            /\ HandOff(Head(LiveAccepts))
       ELSE IF LiveOpens # {}
         THEN \E t \in LiveOpens :
                /\ opens' = LiveOpens \ {t}
                /\ accepts' = LiveAccepts
                /\ stored' = stored
                /\ HandOff(t)
         ELSE /\ stored' = TRUE
              /\ accepts' = LiveAccepts
              /\ opens' = LiveOpens
              /\ UNCHANGED <<caller, dead>>

Done ==
  /\ caller \in {"open", "failed"}
  /\ UNCHANGED vars

Next ==
  \/ Request("accept")
  \/ Request("open")
  \/ Timeout
  \/ PeerRequest
  \/ ActorReceive
  \/ Done

Spec == Init /\ [][Next]_vars

NoDeadHandOff == ~dead
=============================================================================
