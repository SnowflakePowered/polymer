--------------------------- MODULE InputMapHandshake ---------------------------
(***************************************************************************)
(* T7b: input ring handshake (MapOffer / MapAccepted), Windows path.      *)
(*                                                                         *)
(* Rust mapping (src/input/mod.rs, src/input/host/{mod,control}.rs,        *)
(*               src/shared.rs, polymer-hook-input/src/subscriber.rs):     *)
(*   spc[s]     guest InputSubscriber::connect: "connecting", "await" =    *)
(*              control.receive() for MapOffer after sending Hello,        *)
(*              "owning" = Subscriber::from_handle took the File, about to *)
(*              send MapAccepted, "connected" = connect returned Ok,       *)
(*              "failed" = connect returned Err (Subscriber/File dropped), *)
(*              "exited" = InputSubscriber dropped after use, "crashed"    *)
(*   h[s]       the handle Publisher::duplicate_for put into s: "none",    *)
(*              "sub" = in s, unowned, "owned" = Subscriber::_file,        *)
(*              "closed", "gone" = s exited with it                        *)
(*   sock[s], hello[s], offer[s], mapAcc[s]   LocalSocket and Message::    *)
(*              Hello / MapOffer / MapAccepted written                     *)
(*   apc, cur   host control.rs spawn_acceptor thread running              *)
(*              InputIpcListener::accept -> InputPublisher::accept for cur *)
(*   lock       InputIpcListener publication Mutex: held by the acceptor   *)
(*              for the whole InputPublisher::accept, or by on_host_input  *)
(*   input      a host input event waiting for on_host_input               *)
(*   accepted[s] host acceptor already took s from listener.accept()       *)
(*   apc = "dead" the host process exited                                  *)
(*                                                                         *)
(* Subscribers outside FairSubs model a guest that is frozen (suspended,   *)
(* debugged, or hostile): its steps are possible but never forced.         *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Subs, FairSubs, MaxCrashes, MaxHostCrashes, MaxFailures

None == "none"
SubDone == {"failed", "exited", "crashed"}

VARIABLES spc, h, sock, hello, offer, mapAcc, apc, cur, lock, input,
          accepted, crashes, hostCrashes, failures

vars == <<spc, h, sock, hello, offer, mapAcc, apc, cur, lock, input,
          accepted, crashes, hostCrashes, failures>>

msgVars == <<hello, offer, mapAcc>>

TypeOK ==
  /\ spc \in [Subs -> {"connecting", "await", "owning", "connected"} \cup SubDone]
  /\ h \in [Subs -> {"none", "sub", "owned", "closed", "gone"}]
  /\ sock \in [Subs -> {"open", "shut"}]
  /\ apc \in {"idle", "hello", "dup", "send", "await", "dead"}
  /\ cur \in Subs \cup {None}
  /\ lock \in {None, "acceptor", "input"}

Init ==
  /\ spc = [s \in Subs |-> "connecting"]
  /\ h = [s \in Subs |-> "none"]
  /\ sock = [s \in Subs |-> "open"]
  /\ hello = [s \in Subs |-> FALSE]
  /\ offer = [s \in Subs |-> FALSE]
  /\ mapAcc = [s \in Subs |-> FALSE]
  /\ apc = "idle" /\ cur = None /\ lock = None
  /\ input = FALSE
  /\ accepted = [s \in Subs |-> FALSE]
  /\ crashes = 0 /\ hostCrashes = 0 /\ failures = 0

\* InputSubscriber::connect: LocalSocket::connect + send(Hello).
Connect(s) ==
  /\ spc[s] = "connecting"
  /\ spc' = [spc EXCEPT ![s] = "await"]
  /\ hello' = [hello EXCEPT ![s] = TRUE]
  /\ UNCHANGED <<h, sock, offer, mapAcc, apc, cur, lock, input, accepted, crashes, hostCrashes, failures>>

\* control.receive() -> MapOffer, then Subscriber::from_handle.
FromHandle(s) ==
  /\ spc[s] = "await" /\ offer[s]
  /\ h' = [h EXCEPT ![s] = "owned"]
  /\ spc' = [spc EXCEPT ![s] = "owning"]
  /\ UNCHANGED <<sock, msgVars, apc, cur, lock, input, accepted, crashes, hostCrashes, failures>>

\* from_handle: File::from_raw_handle first, then the len/metadata checks
\* fail and drop the File.
FromHandleFails(s) ==
  /\ spc[s] = "await" /\ offer[s] /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ h' = [h EXCEPT ![s] = "closed"]
  /\ spc' = [spc EXCEPT ![s] = "failed"]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, apc, cur, lock, input, accepted, crashes, hostCrashes>>

\* control.receive() returns EOF before a MapOffer.
SubscriberEof(s) ==
  /\ spc[s] = "await" /\ ~offer[s] /\ sock[s] = "shut"
  /\ spc' = [spc EXCEPT ![s] = "failed"]
  /\ UNCHANGED <<h, sock, msgVars, apc, cur, lock, input, accepted, crashes, hostCrashes, failures>>

\* control.send(MapAccepted)?; on Err the Subscriber (and its File) drops.
SendMapAccepted(s) ==
  /\ spc[s] = "owning"
  /\ IF sock[s] = "open"
       THEN /\ mapAcc' = [mapAcc EXCEPT ![s] = TRUE]
            /\ spc' = [spc EXCEPT ![s] = "connected"]
            /\ UNCHANGED h
       ELSE /\ h' = [h EXCEPT ![s] = "closed"]
            /\ spc' = [spc EXCEPT ![s] = "failed"]
            /\ UNCHANGED mapAcc
  /\ UNCHANGED <<sock, hello, offer, apc, cur, lock, input, accepted, crashes, hostCrashes, failures>>

\* The hook's run loop ends and InputSubscriber is dropped.
SubscriberExit(s) ==
  /\ spc[s] = "connected"
  /\ spc' = [spc EXCEPT ![s] = "exited"]
  /\ h' = [h EXCEPT ![s] = "closed"]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, apc, cur, lock, input, accepted, crashes, hostCrashes, failures>>

SubscriberCrash(s) ==
  /\ spc[s] \in {"await", "owning", "connected"} /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ spc' = [spc EXCEPT ![s] = "crashed"]
  /\ h' = [h EXCEPT ![s] = IF @ \in {"sub", "owned"} THEN "gone" ELSE @]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, apc, cur, lock, input, accepted, hostCrashes, failures>>

Alive(s) == spc[s] \notin {"exited", "crashed"}

\* spawn_acceptor: listener.accept(), then publication.lock().accept(socket).
AcceptClient(s) ==
  /\ apc = "idle" /\ lock = None /\ spc[s] # "connecting" /\ ~accepted[s]
  /\ accepted' = [accepted EXCEPT ![s] = TRUE]
  /\ apc' = "hello" /\ cur' = s /\ lock' = "acceptor"
  /\ UNCHANGED <<spc, h, sock, msgVars, input, crashes, hostCrashes, failures>>

\* InputPublisher::accept returns Err: lock released, socket dropped.
AcceptFails ==
  /\ apc' = "idle" /\ cur' = None /\ lock' = None
  /\ sock' = [sock EXCEPT ![cur] = "shut"]

\* control.receive() -> Hello.
ReceiveHello ==
  /\ apc = "hello"
  /\ \/ hello[cur] /\ apc' = "dup" /\ UNCHANGED <<cur, lock, sock>>
     \/ ~hello[cur] /\ sock[cur] = "shut" /\ AcceptFails
  /\ UNCHANGED <<spc, h, msgVars, input, accepted, crashes, hostCrashes, failures>>

\* Publisher::duplicate_for(hello.process_id).
DuplicateFor ==
  /\ apc = "dup"
  /\ IF Alive(cur)
       THEN /\ h' = [h EXCEPT ![cur] = "sub"]
            /\ apc' = "send"
            /\ UNCHANGED <<cur, lock, sock>>
       ELSE /\ AcceptFails /\ UNCHANGED h
  /\ UNCHANGED <<spc, msgVars, input, accepted, crashes, hostCrashes, failures>>

DuplicateForFails ==
  /\ apc = "dup" /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ AcceptFails
  /\ UNCHANGED <<spc, h, msgVars, input, accepted, crashes, hostCrashes>>

\* control.send(MapOffer)?.
SendMapOffer ==
  /\ apc = "send"
  /\ IF sock[cur] = "open"
       THEN /\ offer' = [offer EXCEPT ![cur] = TRUE]
            /\ apc' = "await"
            /\ UNCHANGED <<cur, lock, sock>>
       ELSE AcceptFails /\ UNCHANGED offer
  /\ UNCHANGED <<spc, h, hello, mapAcc, input, accepted, crashes, hostCrashes, failures>>

\* control.receive() != MapAccepted -> Err; otherwise replay and add_client.
ReceiveMapAccepted ==
  /\ apc = "await"
  /\ \/ mapAcc[cur] /\ apc' = "idle" /\ cur' = None /\ lock' = None /\ UNCHANGED sock
     \/ ~mapAcc[cur] /\ sock[cur] = "shut" /\ AcceptFails
  /\ UNCHANGED <<spc, h, msgVars, input, accepted, crashes, hostCrashes, failures>>

HostInputArrives ==
  /\ ~input /\ apc # "dead"
  /\ input' = TRUE
  /\ UNCHANGED <<spc, h, sock, msgVars, apc, cur, lock, accepted, crashes, hostCrashes, failures>>

\* InputIpcListener::on_input -> publication.lock().on_host_input.
OnHostInput ==
  /\ input /\ lock = None
  /\ input' = FALSE
  /\ UNCHANGED <<spc, h, sock, msgVars, apc, cur, lock, accepted, crashes, hostCrashes, failures>>

\* The host process (InputIpcListener owner) dies; handles already
\* duplicated into subscribers stay there.
HostCrash ==
  /\ apc # "dead" /\ hostCrashes < MaxHostCrashes
  /\ hostCrashes' = hostCrashes + 1
  /\ apc' = "dead" /\ cur' = None /\ lock' = None /\ input' = FALSE
  /\ sock' = [s \in Subs |-> "shut"]
  /\ UNCHANGED <<spc, h, msgVars, accepted, crashes, failures>>

Quiescent ==
  /\ \A s \in Subs : spc[s] \in SubDone \cup {"connecting", "connected"}
  /\ \A s \in Subs : spc[s] # "connecting" => accepted[s] \/ apc = "dead"
  /\ apc \in {"idle", "dead"}

Finished == Quiescent /\ ~input /\ UNCHANGED vars

SubStep(s) == Connect(s) \/ FromHandle(s) \/ SubscriberEof(s) \/ SendMapAccepted(s)
AcceptorStep == ReceiveHello \/ DuplicateFor \/ SendMapOffer \/ ReceiveMapAccepted

Next ==
  \/ \E s \in Subs : SubStep(s) \/ FromHandleFails(s) \/ SubscriberExit(s)
                     \/ SubscriberCrash(s) \/ AcceptClient(s)
  \/ AcceptorStep \/ DuplicateForFails \/ HostInputArrives \/ OnHostInput \/ HostCrash
  \/ Finished

Spec == Init /\ [][Next]_vars
        /\ (\A s \in FairSubs : WF_vars(SubStep(s)) /\ WF_vars(SubscriberExit(s)))
        /\ (\A s \in Subs : WF_vars(AcceptClient(s)))
        /\ WF_vars(AcceptorStep) /\ WF_vars(OnHostInput)

\* Handles never end up unowned inside a live subscriber.
NoLeakedHandle == Quiescent => \A s \in Subs : h[s] # "sub"
NoDoubleOwner == \A s \in Subs : h[s] = "owned" => spc[s] \in {"owning", "connected"}

FairSubscriberSettles ==
  \A s \in FairSubs : <>(spc[s] \in SubDone \cup {"connected"})
HostInputProgress == input ~> ~input
=============================================================================
