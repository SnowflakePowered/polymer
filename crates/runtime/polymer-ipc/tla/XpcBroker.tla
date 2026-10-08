----------------------------- MODULE XpcBroker -----------------------------
(***************************************************************************)
(* T6b: per-user directory broker and listener registration lifecycle.    *)
(*                                                                         *)
(* Rust mapping (src/xpc/broker.rs, src/xpc/listener.rs,                   *)
(*               src/xpc/connection.rs, src/control/macos.rs):             *)
(*   registry       Broker's Registry (DashMap<String, endpoint>)          *)
(*   owned[p]       Registration::name of p's DirectorySession             *)
(*   spc[p]         Listener::bind / Listener::close of publisher p:       *)
(*                  "publishing" = Directory::publish awaiting reply,      *)
(*                  "bound" = bind returned Ok, "failed" = bind returned   *)
(*                  Err, "closed" = Listener::close, "crashed"             *)
(*   listenerLive   p's anonymous NSXPCListener not invalidated            *)
(*   dirValid[p]    p's Directory NSXPCConnection not invalidated          *)
(*   req[p]         publish:endpoint:reply: in flight to the broker        *)
(*   entered[p]     DirectorySession::publish entered, register not run    *)
(*                  (only when HandlersSerialized = FALSE)                 *)
(*   status[p]      reply status: 0, EADDRINUSE, or "none"                 *)
(*   closedReg[p]   Registration::close ran (invalidation/interruption     *)
(*                  handler of the broker-side NSXPCConnection)            *)
(*   refusedByDead  ghost: register returned EADDRINUSE for an owner whose *)
(*                  listener was already invalidated                       *)
(*   cpc            LocalSocket::connect_timeout -> Connection::connect:   *)
(*                  Directory::resolve then Connection::activate (ping)    *)
(*                                                                         *)
(* HandlersSerialized = TRUE assumes NSXPC never runs an exported-object   *)
(* method concurrently with, or after, that connection's invalidation      *)
(* handler. FALSE lets DirectorySession::publish be entered before         *)
(* invalidation and take Registration::name after Registration::close.     *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS Pubs, Name, HandlersSerialized, MaxCrashes

None == "none"
Dead == {"failed", "closed", "crashed"}

VARIABLES registry, owned, spc, listenerLive, dirValid, req, entered,
          status, closedReg, cpc, cpeer, crashes, refusedByDead

vars == <<registry, owned, spc, listenerLive, dirValid, req, entered,
          status, closedReg, cpc, cpeer, crashes, refusedByDead>>

TypeOK ==
  /\ registry \in Pubs \cup {None}
  /\ owned \in [Pubs -> {Name, None}]
  /\ spc \in [Pubs -> {"idle", "publishing", "bound"} \cup Dead]
  /\ status \in [Pubs -> {"none", "ok", "EADDRINUSE", "EALREADY"}]
  /\ cpc \in {"resolving", "pinging", "connected", "notfound", "pingfailed"}
  /\ cpeer \in Pubs \cup {None}

Init ==
  /\ registry = None
  /\ owned = [p \in Pubs |-> None]
  /\ spc = [p \in Pubs |-> "idle"]
  /\ listenerLive = [p \in Pubs |-> FALSE]
  /\ dirValid = [p \in Pubs |-> FALSE]
  /\ req = [p \in Pubs |-> FALSE]
  /\ entered = [p \in Pubs |-> FALSE]
  /\ status = [p \in Pubs |-> "none"]
  /\ closedReg = [p \in Pubs |-> FALSE]
  /\ cpc = "resolving" /\ cpeer = None
  /\ crashes = 0 /\ refusedByDead = FALSE

\* Listener::bind: anonymousListener + resume, Directory::connect, polymer_publish.
Bind(p) ==
  /\ spc[p] = "idle"
  /\ spc' = [spc EXCEPT ![p] = "publishing"]
  /\ listenerLive' = [listenerLive EXCEPT ![p] = TRUE]
  /\ dirValid' = [dirValid EXCEPT ![p] = TRUE]
  /\ req' = [req EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<registry, owned, entered, status, closedReg, cpc, cpeer, crashes, refusedByDead>>

\* DirectorySession::register under Registration::name.
RegisterEffect(p) ==
  IF owned[p] # None THEN /\ status' = [status EXCEPT ![p] = "EALREADY"]
                          /\ UNCHANGED <<registry, owned, refusedByDead>>
  ELSE IF registry # None THEN /\ status' = [status EXCEPT ![p] = "EADDRINUSE"]
                               /\ refusedByDead' = (refusedByDead \/ ~listenerLive[registry])
                               /\ UNCHANGED <<registry, owned>>
  ELSE /\ registry' = p
       /\ owned' = [owned EXCEPT ![p] = Name]
       /\ status' = [status EXCEPT ![p] = "ok"]
       /\ UNCHANGED refusedByDead

\* NSXPC delivers publish:endpoint:reply: to DirectorySession::publish.
Publish(p) ==
  /\ req[p] /\ dirValid[p] /\ ~closedReg[p]
  /\ req' = [req EXCEPT ![p] = FALSE]
  /\ IF HandlersSerialized
       THEN RegisterEffect(p) /\ UNCHANGED entered
       ELSE entered' = [entered EXCEPT ![p] = TRUE] /\ UNCHANGED <<registry, owned, status, refusedByDead>>
  /\ UNCHANGED <<spc, listenerLive, dirValid, closedReg, cpc, cpeer, crashes>>

Register(p) ==
  /\ entered[p]
  /\ entered' = [entered EXCEPT ![p] = FALSE]
  /\ RegisterEffect(p)
  /\ UNCHANGED <<spc, listenerLive, dirValid, req, closedReg, cpc, cpeer, crashes>>

\* Directory::publish returns; on Err, Listener::bind invalidates and drops Directory.
PublishReturns(p) ==
  /\ spc[p] = "publishing" /\ status[p] # "none"
  /\ IF status[p] = "ok"
       THEN /\ spc' = [spc EXCEPT ![p] = "bound"]
            /\ UNCHANGED <<listenerLive, dirValid>>
       ELSE /\ spc' = [spc EXCEPT ![p] = "failed"]
            /\ listenerLive' = [listenerLive EXCEPT ![p] = FALSE]
            /\ dirValid' = [dirValid EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<registry, owned, req, entered, status, closedReg, cpc, cpeer, crashes, refusedByDead>>

\* receiver.recv_timeout(TIMEOUT) expires in Directory::publish.
PublishTimeout(p) ==
  /\ spc[p] = "publishing" /\ status[p] = "none"
  /\ spc' = [spc EXCEPT ![p] = "failed"]
  /\ listenerLive' = [listenerLive EXCEPT ![p] = FALSE]
  /\ dirValid' = [dirValid EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<registry, owned, req, entered, status, closedReg, cpc, cpeer, crashes, refusedByDead>>

\* Listener::close -> Inner::close: invalidate, discard, drop Directory.
ListenerClose(p) ==
  /\ spc[p] = "bound"
  /\ spc' = [spc EXCEPT ![p] = "closed"]
  /\ listenerLive' = [listenerLive EXCEPT ![p] = FALSE]
  /\ dirValid' = [dirValid EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<registry, owned, req, entered, status, closedReg, cpc, cpeer, crashes, refusedByDead>>

Crash(p) ==
  /\ spc[p] \in {"publishing", "bound"} /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ spc' = [spc EXCEPT ![p] = "crashed"]
  /\ listenerLive' = [listenerLive EXCEPT ![p] = FALSE]
  /\ dirValid' = [dirValid EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<registry, owned, req, entered, status, closedReg, cpc, cpeer, refusedByDead>>

\* BrokerDelegate::accept handler -> Registration::close.
RegistrationClose(p) ==
  /\ spc[p] # "idle" /\ ~dirValid[p] /\ ~closedReg[p]
  /\ closedReg' = [closedReg EXCEPT ![p] = TRUE]
  /\ req' = [req EXCEPT ![p] = FALSE]
  /\ IF owned[p] # None
       THEN /\ registry' = None
            /\ owned' = [owned EXCEPT ![p] = None]
       ELSE UNCHANGED <<registry, owned>>
  /\ UNCHANGED <<spc, listenerLive, dirValid, entered, status, cpc, cpeer, crashes, refusedByDead>>

\* Directory::resolve; NotFound is retried by connect_timeout until the deadline.
Resolve ==
  /\ cpc = "resolving" /\ registry # None
  /\ cpc' = "pinging" /\ cpeer' = registry
  /\ UNCHANGED <<registry, owned, spc, listenerLive, dirValid, req, entered, status, closedReg, crashes, refusedByDead>>

ConnectDeadline ==
  /\ cpc = "resolving" /\ registry = None
  /\ cpc' = "notfound"
  /\ UNCHANGED <<registry, owned, spc, listenerLive, dirValid, req, entered, status, closedReg, cpeer, crashes, refusedByDead>>

\* Connection::activate: polymer_ping; the error is not NotFound, so no retry.
Activate ==
  /\ cpc = "pinging"
  /\ cpc' = IF listenerLive[cpeer] THEN "connected" ELSE "pingfailed"
  /\ UNCHANGED <<registry, owned, spc, listenerLive, dirValid, req, entered, status, closedReg, cpeer, crashes, refusedByDead>>

Finished ==
  /\ \A p \in Pubs : spc[p] \in Dead \cup {"bound"} /\ (spc[p] \in Dead => closedReg[p]) /\ ~entered[p]
  /\ cpc \in {"connected", "notfound", "pingfailed"}
  /\ UNCHANGED vars

Next ==
  \/ \E p \in Pubs : Bind(p) \/ Publish(p) \/ Register(p) \/ PublishReturns(p)
                     \/ PublishTimeout(p) \/ ListenerClose(p) \/ Crash(p) \/ RegistrationClose(p)
  \/ Resolve \/ ConnectDeadline \/ Activate
  \/ Finished

Spec == Init /\ [][Next]_vars
        /\ (\A p \in Pubs : WF_vars(Bind(p)) /\ WF_vars(Publish(p)) /\ WF_vars(Register(p))
                            /\ WF_vars(PublishReturns(p)) /\ WF_vars(RegistrationClose(p)))
        /\ WF_vars(Resolve) /\ WF_vars(Activate)

OwnerConsistent == registry # None => owned[registry] = Name

\* Once a session's Registration::close ran, its name is never published again.
NoStaleRegistration == registry # None => ~closedReg[registry]

\* A rebind is refused only by a live owner.
RebindNotBlockedByDeadOwner == ~refusedByDead

RegistrationReleased ==
  \A p \in Pubs : spc[p] \in Dead ~> registry # p
=============================================================================
