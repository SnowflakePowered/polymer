---------------------------- MODULE L2capConfig ----------------------------
(***************************************************************************)
(* T9: L2CAP configuration negotiation for one channel.                   *)
(*                                                                         *)
(* Host side = link_actor in src/transport/bredr/l2cap/link.rs, with       *)
(* ConfigurationOption::negotiate from l2cap/signaling.rs.  Peer side is   *)
(* an abstract Core-spec L2CAP endpoint (Vol 3 Part A 4.4-4.5, 7.1) whose  *)
(* optional behaviours are chosen nondeterministically in Init.           *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   hState     "pending" = entry in `pending` (PendingChannel),           *)
(*              "open"    = moved into `channels` by finish_if_ready,      *)
(*              "failed"  = removed by fail_pending,                       *)
(*              "closed"  = removed on DisconnectionRequest / Close        *)
(*   localCfg   PendingChannel::local_configured                           *)
(*   remoteCfg  PendingChannel::remote_configured                          *)
(*   caller     the request_channel oneshot: waiting, channel (Ok),        *)
(*              error (Err), timedout (OPEN/ACCEPT_CHANNEL_TIMEOUT fired,  *)
(*              receiver dropped, pending entry left in place)             *)
(*   toPeer / toHost   signaling PDUs on the ACL (send_signal / frames)    *)
(*   msg.good   ConfigurationResponse source_cid names the peer's CID      *)
(*              (pending lookup hit) rather than echoing destination_cid   *)
(*                                                                         *)
(* Host actions: HostReceiveConfigurationResponse / ...Request /           *)
(* ...DisconnectionRequest are the matching arms of link_actor's frame     *)
(* branch; FinishIfReady is finish_if_ready (a Channel produced for a      *)
(* timed-out caller is dropped, whose Drop sends LinkCommand::Close ->     *)
(* DisconnectionRequest); RequestChannelTimeout is request_channel's       *)
(* time::timeout.  The host's single option-free ConfigurationRequest is   *)
(* sent by accept_request / the ConnectionResponse arm (Init).             *)
(*                                                                         *)
(* Abstracted: connection setup, crossed-request coalescing, CIDs and      *)
(* identifiers (one channel), option values (MTU / QoS / hint options are  *)
(* always accepted), Command Reject, RTX retransmission.                   *)
(***************************************************************************)
EXTENDS Naturals, Sequences

CONSTANT PeerBehaviours

ASSUME PeerBehaviours \subseteq {"fragment", "unknown", "reject", "contrsp", "renegotiate"}

May(b) == IF b \in PeerBehaviours THEN BOOLEAN ELSE {FALSE}

Msg(t, r, c, u, g) == [type |-> t, result |-> r, cont |-> c, unknown |-> u, good |-> g]

VARIABLES hState, localCfg, remoteCfg, caller,
          pState, pOutDone, pInDone, pReqNeeded, pWaitRsp, pFrags, pUnknown,
          pReject, pContRsp, pRenegotiate,
          toPeer, toHost

vars == <<hState, localCfg, remoteCfg, caller, pState, pOutDone, pInDone, pReqNeeded,
          pWaitRsp, pFrags, pUnknown, pReject, pContRsp, pRenegotiate, toPeer, toHost>>

hostVars == <<hState, localCfg, remoteCfg, caller>>
peerVars == <<pState, pOutDone, pInDone, pReqNeeded, pWaitRsp, pFrags, pUnknown,
              pReject, pContRsp, pRenegotiate>>

TypeOK ==
  /\ hState \in {"pending", "open", "failed", "closed"}
  /\ caller \in {"waiting", "channel", "error", "timedout"}
  /\ pState \in {"config", "open", "closed"}
  /\ pFrags \in 1..2

Init ==
  /\ hState = "pending" /\ localCfg = FALSE /\ remoteCfg = FALSE /\ caller = "waiting"
  /\ pState = "config" /\ pOutDone = FALSE /\ pInDone = FALSE
  /\ pReqNeeded = TRUE /\ pWaitRsp = FALSE
  /\ pFrags \in IF "fragment" \in PeerBehaviours THEN 1..2 ELSE {1}
  /\ pUnknown \in May("unknown")
  /\ pReject \in May("reject")
  /\ pContRsp \in May("contrsp")
  /\ pRenegotiate \in May("renegotiate")
  /\ toPeer = <<Msg("ConfReq", 0, FALSE, FALSE, TRUE)>>
  /\ toHost = <<>>

----------------------------------------------------------------------------
\* Host (link_actor)

FinishIfReady(lc, rc) ==
  IF hState = "pending" /\ lc /\ rc
    THEN IF caller = "timedout"
           THEN /\ hState' = "closed"
                /\ toPeer' = Append(toPeer, Msg("DiscReq", 0, FALSE, FALSE, TRUE))
                /\ UNCHANGED caller
           ELSE /\ hState' = "open"
                /\ caller' = IF caller = "waiting" THEN "channel" ELSE caller
                /\ UNCHANGED toPeer
    ELSE UNCHANGED <<hState, caller, toPeer>>

HostReceiveConfigurationResponse ==
  /\ toHost # <<>> /\ Head(toHost).type = "ConfRsp"
  /\ LET m == Head(toHost) IN
     /\ toHost' = Tail(toHost)
     /\ IF hState # "pending"
          THEN UNCHANGED <<hState, localCfg, caller, toPeer>>
        ELSE IF m.result # 0
          THEN /\ hState' = "failed"
               /\ caller' = IF caller = "waiting" THEN "error" ELSE caller
               /\ UNCHANGED <<localCfg, toPeer>>
        ELSE /\ localCfg' = ~m.cont
             /\ FinishIfReady(~m.cont, remoteCfg)
  /\ UNCHANGED <<remoteCfg>>
  /\ UNCHANGED peerVars

HostReceiveConfigurationRequest ==
  /\ toHost # <<>> /\ Head(toHost).type = "ConfReq"
  /\ LET m == Head(toHost)
         result == IF m.unknown THEN 3 ELSE 0
         rsp == Msg("ConfRsp", result, m.cont, FALSE, hState = "pending")
     IN
     /\ toHost' = Tail(toHost)
     /\ IF result = 0 /\ hState = "pending"
          THEN /\ remoteCfg' = ~m.cont
               /\ IF localCfg /\ ~m.cont
                    THEN IF caller = "timedout"
                           THEN /\ hState' = "closed"
                                /\ toPeer' = toPeer \o <<rsp, Msg("DiscReq", 0, FALSE, FALSE, TRUE)>>
                                /\ UNCHANGED caller
                           ELSE /\ hState' = "open"
                                /\ caller' = IF caller = "waiting" THEN "channel" ELSE caller
                                /\ toPeer' = Append(toPeer, rsp)
                    ELSE /\ toPeer' = Append(toPeer, rsp)
                         /\ UNCHANGED <<hState, caller>>
          ELSE /\ toPeer' = Append(toPeer, rsp)
               /\ UNCHANGED <<hState, remoteCfg, caller>>
  /\ UNCHANGED <<localCfg>>
  /\ UNCHANGED peerVars

HostReceiveDisconnectionRequest ==
  /\ toHost # <<>> /\ Head(toHost).type = "DiscReq"
  /\ toHost' = Tail(toHost)
  /\ hState' = IF hState = "failed" THEN "failed" ELSE "closed"
  /\ caller' = IF caller = "waiting" THEN "error" ELSE caller
  /\ toPeer' = Append(toPeer, Msg("DiscRsp", 0, FALSE, FALSE, TRUE))
  /\ UNCHANGED <<localCfg, remoteCfg>>
  /\ UNCHANGED peerVars

HostReceiveOther ==
  /\ toHost # <<>> /\ Head(toHost).type = "DiscRsp"
  /\ toHost' = Tail(toHost)
  /\ UNCHANGED <<hostVars, peerVars, toPeer>>

RequestChannelTimeout ==
  /\ caller = "waiting"
  /\ caller' = "timedout"
  /\ UNCHANGED <<hState, localCfg, remoteCfg, peerVars, toPeer, toHost>>

----------------------------------------------------------------------------
\* Peer (abstract spec-compliant L2CAP)

PeerUnchangedChoices == UNCHANGED <<pReject, pContRsp, pRenegotiate>>

PeerSendConfigurationRequest ==
  /\ pState = "config" /\ pReqNeeded /\ ~pWaitRsp
  /\ toHost' = Append(toHost, Msg("ConfReq", 0, pFrags > 1, pUnknown, TRUE))
  /\ pReqNeeded' = FALSE /\ pWaitRsp' = TRUE
  /\ UNCHANGED <<pState, pOutDone, pInDone, pFrags, pUnknown>>
  /\ PeerUnchangedChoices
  /\ UNCHANGED <<hostVars, toPeer>>

PeerOpen(outDone, inDone) ==
  pState' = IF pState = "config" /\ outDone /\ inDone THEN "open" ELSE pState

PeerReceiveConfigurationResponse ==
  /\ toPeer # <<>> /\ Head(toPeer).type = "ConfRsp"
  /\ LET m == Head(toPeer) IN
     /\ toPeer' = Tail(toPeer)
     /\ IF pState # "config" \/ ~m.good \/ ~pWaitRsp
          THEN UNCHANGED <<pState, pOutDone, pReqNeeded, pWaitRsp, pFrags, pUnknown>>
        ELSE IF m.result = 3
          THEN /\ pUnknown' = FALSE /\ pReqNeeded' = TRUE /\ pWaitRsp' = FALSE
               /\ UNCHANGED <<pState, pOutDone, pFrags>>
        ELSE IF pFrags > 1
          THEN /\ pFrags' = pFrags - 1 /\ pReqNeeded' = TRUE /\ pWaitRsp' = FALSE
               /\ UNCHANGED <<pState, pOutDone, pUnknown>>
        ELSE /\ pOutDone' = TRUE /\ pWaitRsp' = FALSE
             /\ PeerOpen(TRUE, pInDone)
             /\ UNCHANGED <<pReqNeeded, pFrags, pUnknown>>
  /\ UNCHANGED <<pInDone>>
  /\ PeerUnchangedChoices
  /\ UNCHANGED <<hostVars, toHost>>

PeerReceiveConfigurationRequest ==
  /\ toPeer # <<>> /\ Head(toPeer).type = "ConfReq"
  /\ toPeer' = Tail(toPeer)
  /\ IF pState # "config"
       THEN /\ UNCHANGED <<pState, pInDone, pReject, pContRsp, toHost>>
     ELSE IF pReject
       THEN /\ toHost' = Append(toHost, Msg("ConfRsp", 1, FALSE, FALSE, TRUE))
            /\ pReject' = FALSE
            /\ UNCHANGED <<pState, pInDone, pContRsp>>
     ELSE IF pContRsp
       THEN /\ toHost' = Append(toHost, Msg("ConfRsp", 0, TRUE, FALSE, TRUE))
            /\ pContRsp' = FALSE
            /\ UNCHANGED <<pState, pInDone, pReject>>
     ELSE /\ toHost' = Append(toHost, Msg("ConfRsp", 0, FALSE, FALSE, TRUE))
          /\ pInDone' = TRUE
          /\ PeerOpen(pOutDone, TRUE)
          /\ UNCHANGED <<pReject, pContRsp>>
  /\ UNCHANGED <<pOutDone, pReqNeeded, pWaitRsp, pFrags, pUnknown, pRenegotiate>>
  /\ UNCHANGED hostVars

PeerReceiveDisconnectionRequest ==
  /\ toPeer # <<>> /\ Head(toPeer).type = "DiscReq"
  /\ toPeer' = Tail(toPeer)
  /\ toHost' = IF pState # "closed" THEN Append(toHost, Msg("DiscRsp", 0, FALSE, FALSE, TRUE))
                                     ELSE toHost
  /\ pState' = "closed"
  /\ UNCHANGED <<pOutDone, pInDone, pReqNeeded, pWaitRsp, pFrags, pUnknown>>
  /\ PeerUnchangedChoices
  /\ UNCHANGED hostVars

PeerReceiveOther ==
  /\ toPeer # <<>> /\ Head(toPeer).type = "DiscRsp"
  /\ toPeer' = Tail(toPeer)
  /\ UNCHANGED <<hostVars, peerVars, toHost>>

\* Reconfiguration of an open channel (Core spec 4.4: either side may).
PeerRenegotiate ==
  /\ pState = "open" /\ pRenegotiate
  /\ pRenegotiate' = FALSE
  /\ pState' = "config" /\ pOutDone' = FALSE /\ pInDone' = TRUE
  /\ pReqNeeded' = TRUE /\ pWaitRsp' = FALSE
  /\ UNCHANGED <<pFrags, pUnknown, pReject, pContRsp>>
  /\ UNCHANGED <<hostVars, toPeer, toHost>>

\* The peer's configuration timeout: it gives up and disconnects.
PeerConfigTimeout ==
  /\ pState = "config"
  /\ pState' = "closed"
  /\ toHost' = Append(toHost, Msg("DiscReq", 0, FALSE, FALSE, TRUE))
  /\ UNCHANGED <<pOutDone, pInDone, pReqNeeded, pWaitRsp, pFrags, pUnknown>>
  /\ PeerUnchangedChoices
  /\ UNCHANGED <<hostVars, toPeer>>

----------------------------------------------------------------------------

HostDone == hState \in {"open", "failed", "closed"}
PeerDone == pState \in {"open", "closed"}
Settled == HostDone /\ PeerDone /\ toPeer = <<>> /\ toHost = <<>>

Finished == Settled /\ UNCHANGED vars

HostStep ==
  \/ HostReceiveConfigurationResponse \/ HostReceiveConfigurationRequest
  \/ HostReceiveDisconnectionRequest \/ HostReceiveOther

PeerStep ==
  \/ PeerSendConfigurationRequest \/ PeerReceiveConfigurationResponse
  \/ PeerReceiveConfigurationRequest \/ PeerReceiveDisconnectionRequest
  \/ PeerReceiveOther \/ PeerRenegotiate

Next ==
  \/ HostStep \/ PeerStep
  \/ RequestChannelTimeout \/ PeerConfigTimeout
  \/ Finished

\* Protocol steps are fair; neither side's timeout is.
Spec == Init /\ [][Next]_vars /\ WF_vars(HostStep) /\ WF_vars(PeerStep)

\* With timeouts as the only fallback, the spec including them.
SpecWithTimeouts ==
  Spec /\ WF_vars(RequestChannelTimeout) /\ WF_vars(PeerConfigTimeout)

\* Once nothing is in flight and both sides are done, they agree on whether
\* the channel is open.
Agreement == Settled => ((hState = "open") = (pState = "open"))

\* The host never treats the channel as open while the peer is still
\* configuring with nothing left in flight.
NoHalfOpen == (toPeer = <<>> /\ toHost = <<>>) => ~(hState = "open" /\ pState = "config")

\* Negotiation finishes without relying on a timeout.
NegotiationTerminates == <>[]Settled

=============================================================================
