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
(*   hpc[s]     host "polymer-input-ipc-client" thread for s, spawned by   *)
(*              Control::spawn_acceptor and running InputIpcListener::serve:*)
(*              "new" = not accepted, "reg"/"chk" = Control::handshake     *)
(*              inserting into `handshaking` then checking `stopping`,     *)
(*              "hello" .. "replaying" = InputIpcListener::accept,         *)
(*              "untrack" = handshake removes s from `handshaking`,        *)
(*              "push"/"chk2" = Control::add_client pushing the writer and *)
(*              checking `stopping`, "feedback" = forward_feedback loop    *)
(*   tracked[s], client[s]   s in Control::handshaking / Control::clients  *)
(*   lock       InputIpcListener publication Mutex: held by one client     *)
(*              thread during duplicate_for or replay, or by on_host_input *)
(*   input      a host input event waiting for on_host_input               *)
(*   stopping   Control::shutdown has set `stopping`                       *)
(*   closed     Control::shutdown has shut every tracked/client socket and *)
(*              closed the listener                                        *)
(*   pending    clients a Control::send topology broadcast still writes to *)
(*   alive      the host process is running                                *)
(*                                                                         *)
(* Subscribers outside FairSubs model a guest that is frozen (suspended,   *)
(* debugged, or hostile): its steps, including reading its control socket, *)
(* are possible but never forced.                                          *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Subs, FairSubs, MaxCrashes, MaxHostCrashes, MaxFailures, WriteTimeout

None == "none"
SubDone == {"failed", "exited", "crashed"}
HostIdle == {"new", "feedback", "done"}

VARIABLES spc, h, sock, hello, offer, mapAcc, hpc, tracked, client, lock, input,
          stopping, closed, pending, alive, crashes, hostCrashes, failures

vars == <<spc, h, sock, hello, offer, mapAcc, hpc, tracked, client, lock, input,
          stopping, closed, pending, alive, crashes, hostCrashes, failures>>

msgVars == <<hello, offer, mapAcc>>
hostVars == <<hpc, tracked, client, lock, stopping, closed, pending, alive>>
counters == <<crashes, hostCrashes, failures>>

TypeOK ==
  /\ spc \in [Subs -> {"connecting", "await", "owning", "connected"} \cup SubDone]
  /\ h \in [Subs -> {"none", "sub", "owned", "closed", "gone"}]
  /\ sock \in [Subs -> {"open", "shut"}]
  /\ hpc \in [Subs -> {"new", "reg", "chk", "hello", "dup", "send", "await", "replay",
                       "replaying", "untrack", "push", "chk2", "feedback", "done"}]
  /\ tracked \in [Subs -> BOOLEAN]
  /\ client \in [Subs -> BOOLEAN]
  /\ lock \in Subs \cup {None, "input"}
  /\ pending \subseteq Subs

Init ==
  /\ spc = [s \in Subs |-> "connecting"]
  /\ h = [s \in Subs |-> "none"]
  /\ sock = [s \in Subs |-> "open"]
  /\ hello = [s \in Subs |-> FALSE]
  /\ offer = [s \in Subs |-> FALSE]
  /\ mapAcc = [s \in Subs |-> FALSE]
  /\ hpc = [s \in Subs |-> "new"]
  /\ tracked = [s \in Subs |-> FALSE]
  /\ client = [s \in Subs |-> FALSE]
  /\ lock = None /\ input = FALSE
  /\ stopping = FALSE /\ closed = FALSE /\ pending = {} /\ alive = TRUE
  /\ crashes = 0 /\ hostCrashes = 0 /\ failures = 0

-----------------------------------------------------------------------------
(* Guest *)

\* InputSubscriber::connect: LocalSocket::connect + send(Hello).
Connect(s) ==
  /\ spc[s] = "connecting"
  /\ spc' = [spc EXCEPT ![s] = "await"]
  /\ hello' = [hello EXCEPT ![s] = TRUE]
  /\ UNCHANGED <<h, sock, offer, mapAcc, hostVars, input, counters>>

\* control.receive() -> MapOffer, then Subscriber::from_handle.
FromHandle(s) ==
  /\ spc[s] = "await" /\ offer[s]
  /\ h' = [h EXCEPT ![s] = "owned"]
  /\ spc' = [spc EXCEPT ![s] = "owning"]
  /\ UNCHANGED <<sock, msgVars, hostVars, input, counters>>

\* from_handle: File::from_raw_handle first, then the len/metadata checks
\* fail and drop the File.
FromHandleFails(s) ==
  /\ spc[s] = "await" /\ offer[s] /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ h' = [h EXCEPT ![s] = "closed"]
  /\ spc' = [spc EXCEPT ![s] = "failed"]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, hostVars, input, crashes, hostCrashes>>

\* control.receive() returns EOF before a MapOffer.
SubscriberEof(s) ==
  /\ spc[s] = "await" /\ ~offer[s] /\ sock[s] = "shut"
  /\ spc' = [spc EXCEPT ![s] = "failed"]
  /\ UNCHANGED <<h, sock, msgVars, hostVars, input, counters>>

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
  /\ UNCHANGED <<sock, hello, offer, hostVars, input, counters>>

\* The hook's run loop ends and InputSubscriber is dropped.
SubscriberExit(s) ==
  /\ spc[s] = "connected"
  /\ spc' = [spc EXCEPT ![s] = "exited"]
  /\ h' = [h EXCEPT ![s] = "closed"]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, hostVars, input, counters>>

SubscriberCrash(s) ==
  /\ spc[s] \in {"await", "owning", "connected"} /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ spc' = [spc EXCEPT ![s] = "crashed"]
  /\ h' = [h EXCEPT ![s] = IF @ \in {"sub", "owned"} THEN "gone" ELSE @]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<msgVars, hostVars, input, hostCrashes, failures>>

Alive(s) == spc[s] \notin {"exited", "crashed"}

-----------------------------------------------------------------------------
(* Host client thread for s *)

Goto(s, pc) == hpc' = [hpc EXCEPT ![s] = pc]

\* serve: the error is logged and the socket dropped; Control::handshake
\* removes s from `handshaking` on every exit.
Fail(s) ==
  /\ Goto(s, "done")
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ tracked' = [tracked EXCEPT ![s] = FALSE]

\* spawn_acceptor: listener.accept(), then spawn the client thread.
Accept(s) ==
  /\ alive /\ ~closed /\ hpc[s] = "new" /\ spc[s] # "connecting"
  /\ Goto(s, "reg")
  /\ UNCHANGED <<spc, h, sock, msgVars, tracked, client, lock, input, stopping,
                 closed, pending, alive, counters>>

Register(s) ==
  /\ hpc[s] = "reg"
  /\ tracked' = [tracked EXCEPT ![s] = TRUE]
  /\ Goto(s, "chk")
  /\ UNCHANGED <<spc, h, sock, msgVars, client, lock, input, stopping, closed,
                 pending, alive, counters>>

CheckStopping(s) ==
  /\ hpc[s] = "chk"
  /\ IF stopping THEN Fail(s) ELSE Goto(s, "hello") /\ UNCHANGED <<sock, tracked>>
  /\ UNCHANGED <<spc, h, msgVars, client, lock, input, stopping, closed, pending,
                 alive, counters>>

\* socket.receive() -> Hello.
ReceiveHello(s) ==
  /\ hpc[s] = "hello"
  /\ \/ hello[s] /\ Goto(s, "dup") /\ UNCHANGED <<sock, tracked>>
     \/ ~hello[s] /\ sock[s] = "shut" /\ Fail(s)
  /\ UNCHANGED <<spc, h, msgVars, client, lock, input, stopping, closed, pending,
                 alive, counters>>

\* publication.lock().map_offer(&hello): the lock is held only while
\* Publisher::duplicate_for runs.
DuplicateFor(s) ==
  /\ hpc[s] = "dup" /\ lock = None
  /\ IF Alive(s)
       THEN /\ h' = [h EXCEPT ![s] = "sub"]
            /\ Goto(s, "send")
            /\ UNCHANGED <<sock, tracked>>
       ELSE Fail(s) /\ UNCHANGED h
  /\ UNCHANGED <<spc, msgVars, client, lock, input, stopping, closed, pending,
                 alive, counters>>

DuplicateForFails(s) ==
  /\ hpc[s] = "dup" /\ lock = None /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ Fail(s)
  /\ UNCHANGED <<spc, h, msgVars, client, lock, input, stopping, closed, pending,
                 alive, crashes, hostCrashes>>

\* socket.send(MapOffer)?, without the publication lock.
SendMapOffer(s) ==
  /\ hpc[s] = "send"
  /\ IF sock[s] = "open"
       THEN /\ offer' = [offer EXCEPT ![s] = TRUE]
            /\ Goto(s, "await")
            /\ UNCHANGED <<sock, tracked>>
       ELSE Fail(s) /\ UNCHANGED offer
  /\ UNCHANGED <<spc, h, hello, mapAcc, client, lock, input, stopping, closed,
                 pending, alive, counters>>

\* socket.receive() != MapAccepted -> Err, without the publication lock.
ReceiveMapAccepted(s) ==
  /\ hpc[s] = "await"
  /\ \/ mapAcc[s] /\ Goto(s, "replay") /\ UNCHANGED <<sock, tracked>>
     \/ ~mapAcc[s] /\ sock[s] = "shut" /\ Fail(s)
  /\ UNCHANGED <<spc, h, msgVars, client, lock, input, stopping, closed, pending,
                 alive, counters>>

\* publication.lock().replay(): publishes into the shared ring only.
LockReplay(s) ==
  /\ hpc[s] = "replay" /\ lock = None
  /\ lock' = s
  /\ Goto(s, "replaying")
  /\ UNCHANGED <<spc, h, sock, msgVars, tracked, client, input, stopping, closed,
                 pending, alive, counters>>

Replay(s) ==
  /\ hpc[s] = "replaying"
  /\ lock' = None
  /\ Goto(s, "untrack")
  /\ UNCHANGED <<spc, h, sock, msgVars, tracked, client, input, stopping, closed,
                 pending, alive, counters>>

Untrack(s) ==
  /\ hpc[s] = "untrack"
  /\ tracked' = [tracked EXCEPT ![s] = FALSE]
  /\ Goto(s, "push")
  /\ UNCHANGED <<spc, h, sock, msgVars, client, lock, input, stopping, closed,
                 pending, alive, counters>>

Push(s) ==
  /\ hpc[s] = "push"
  /\ client' = [client EXCEPT ![s] = TRUE]
  /\ Goto(s, "chk2")
  /\ UNCHANGED <<spc, h, sock, msgVars, tracked, lock, input, stopping, closed,
                 pending, alive, counters>>

CheckStoppingAfterPush(s) ==
  /\ hpc[s] = "chk2"
  /\ sock' = [sock EXCEPT ![s] = IF stopping THEN "shut" ELSE @]
  /\ Goto(s, "feedback")
  /\ UNCHANGED <<spc, h, msgVars, tracked, client, lock, input, stopping, closed,
                 pending, alive, counters>>

\* forward_feedback: reader.receive() fails once the socket is shut.
FeedbackEnds(s) ==
  /\ hpc[s] = "feedback" /\ sock[s] = "shut"
  /\ Goto(s, "done")
  /\ UNCHANGED <<spc, h, sock, msgVars, tracked, client, lock, input, stopping,
                 closed, pending, alive, counters>>

HostStep(s) ==
  \/ Register(s) \/ CheckStopping(s) \/ ReceiveHello(s) \/ DuplicateFor(s)
  \/ SendMapOffer(s) \/ ReceiveMapAccepted(s) \/ LockReplay(s) \/ Replay(s)
  \/ Untrack(s) \/ Push(s) \/ CheckStoppingAfterPush(s) \/ FeedbackEnds(s)

-----------------------------------------------------------------------------
(* Host input, topology broadcast, shutdown *)

HostInputArrives ==
  /\ ~input /\ alive
  /\ input' = TRUE
  /\ UNCHANGED <<spc, h, sock, msgVars, hostVars, counters>>

\* InputIpcListener::on_input -> publication.lock().on_host_input.
OnHostInput ==
  /\ input /\ lock = None
  /\ input' = FALSE
  /\ UNCHANGED <<spc, h, sock, msgVars, hostVars, counters>>

\* Control::publish_topology -> Control::send to every registered client.
TopologyArrives ==
  /\ alive /\ pending = {}
  /\ pending' = {s \in Subs : client[s]}
  /\ pending' # {}
  /\ UNCHANGED <<spc, h, sock, msgVars, hpc, tracked, client, lock, input,
                 stopping, closed, alive, counters>>

\* The write completes once the guest drains its control socket.
TopologyRead(s) ==
  /\ s \in pending /\ sock[s] = "open" /\ spc[s] = "connected"
  /\ pending' = pending \ {s}
  /\ UNCHANGED <<spc, h, sock, msgVars, hpc, tracked, client, lock, input,
                 stopping, closed, alive, counters>>

\* A failed or timed out write evicts the client and shuts its socket.
TopologyWriteFails(s) ==
  /\ s \in pending /\ (WriteTimeout \/ sock[s] = "shut")
  /\ pending' = pending \ {s}
  /\ client' = [client EXCEPT ![s] = FALSE]
  /\ sock' = [sock EXCEPT ![s] = "shut"]
  /\ UNCHANGED <<spc, h, msgVars, hpc, tracked, lock, input, stopping, closed,
                 alive, counters>>

\* Control::shutdown: `stopping`, then shut every handshaking and client
\* socket. InputIpcListener::shutdown closes the listener first.
ShutdownStarts ==
  /\ alive /\ ~stopping
  /\ stopping' = TRUE
  /\ UNCHANGED <<spc, h, sock, msgVars, hpc, tracked, client, lock, input, closed,
                 pending, alive, counters>>

ShutdownCloses ==
  /\ stopping /\ ~closed /\ alive
  /\ closed' = TRUE
  /\ sock' = [s \in Subs |-> IF tracked[s] \/ client[s] \/ hpc[s] = "new"
                               THEN "shut" ELSE sock[s]]
  /\ client' = [s \in Subs |-> FALSE]
  /\ UNCHANGED <<spc, h, msgVars, hpc, tracked, lock, input, stopping, pending,
                 alive, counters>>

\* The host process dies; handles already duplicated into subscribers stay
\* there.
HostCrash ==
  /\ alive /\ hostCrashes < MaxHostCrashes
  /\ hostCrashes' = hostCrashes + 1
  /\ alive' = FALSE /\ closed' = TRUE /\ lock' = None /\ input' = FALSE
  /\ pending' = {}
  /\ hpc' = [s \in Subs |-> IF hpc[s] = "new" THEN "new" ELSE "done"]
  /\ tracked' = [s \in Subs |-> FALSE]
  /\ client' = [s \in Subs |-> FALSE]
  /\ sock' = [s \in Subs |-> "shut"]
  /\ UNCHANGED <<spc, h, msgVars, stopping, crashes, failures>>

-----------------------------------------------------------------------------

Quiescent ==
  /\ \A s \in Subs : spc[s] \in SubDone \cup {"connecting", "connected"}
  /\ \A s \in Subs : hpc[s] \in HostIdle
  /\ \A s \in Subs : spc[s] # "connecting" => hpc[s] # "new" \/ closed

Finished == Quiescent /\ ~input /\ pending = {} /\ UNCHANGED vars

SubStep(s) == Connect(s) \/ FromHandle(s) \/ SubscriberEof(s) \/ SendMapAccepted(s)

Next ==
  \/ \E s \in Subs : \/ SubStep(s) \/ FromHandleFails(s) \/ SubscriberExit(s)
                     \/ SubscriberCrash(s) \/ Accept(s) \/ HostStep(s)
                     \/ DuplicateForFails(s) \/ TopologyRead(s) \/ TopologyWriteFails(s)
  \/ HostInputArrives \/ OnHostInput \/ TopologyArrives
  \/ ShutdownStarts \/ ShutdownCloses \/ HostCrash
  \/ Finished

Spec == Init /\ [][Next]_vars
        /\ (\A s \in FairSubs : /\ WF_vars(SubStep(s)) /\ WF_vars(SubscriberExit(s))
                                /\ WF_vars(TopologyRead(s)))
        /\ (\A s \in Subs : WF_vars(Accept(s)) /\ WF_vars(HostStep(s))
                            /\ WF_vars(TopologyWriteFails(s)))
        /\ WF_vars(OnHostInput) /\ WF_vars(ShutdownCloses)

\* Handles never end up unowned inside a live subscriber while the host runs.
\* A host that crashes or shuts down between duplicate_for and MapOffer leaves
\* the handle in a guest that is being torn down with it.
NoLeakedHandle ==
  (Quiescent /\ alive /\ ~stopping) => \A s \in Subs : h[s] # "sub"
NoDoubleOwner == \A s \in Subs : h[s] = "owned" => spc[s] \in {"owning", "connected"}
LockNeverAcrossSocketWait ==
  \A s \in Subs : lock = s => hpc[s] = "replaying"

FairSubscriberSettles ==
  \A s \in FairSubs : <>(spc[s] \in SubDone \cup {"connected"})
HostInputProgress == input ~> ~input
TopologyProgress == (pending # {}) ~> (pending = {})
ShutdownCompletes ==
  (stopping /\ alive) ~> (~alive \/ \A s \in Subs : hpc[s] \in {"new", "done"})
=============================================================================
