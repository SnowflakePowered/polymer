------------------------------ MODULE XpcQueue ------------------------------
(***************************************************************************)
(* T6a: Queue<Call> behind one NSXPC connection and its call primitive.   *)
(*                                                                         *)
(* Rust mapping (src/xpc/queue.rs, src/xpc/connection.rs,                  *)
(*               polymer-vfs-fskit/src/server.rs, extension/connection.rs):*)
(*   items       Queue::state.0 (VecDeque) of Incoming::calls, cap Cap     *)
(*               (64 in Rust)                                              *)
(*   closed      Queue::state.1                                            *)
(*   rpc[r]      a thread looping Session::serve -> Connection::           *)
(*               receive_call(None) -> Queue::receive -> Call::reply;      *)
(*               "check" = loop top holding the mutex, "wait" = parked in  *)
(*               Condvar::wait (Queue::ready), "serve" = owns a Call,      *)
(*               "eof" = receive returned UnexpectedEof, "done" = worker   *)
(*               ran connection.close() after serve returned               *)
(*   serving[r]  the Call owned by rpc[r] = "serve"                        *)
(*   cpc[a]      a client thread in Connection::call (fskit                *)
(*               extension::Connection::call)                              *)
(*   inflight    proxy messages sent by polymer_call, not yet delivered to *)
(*               Receiver::call on the peer                                *)
(*   reply[a]    first value try_sent into the call's sync_channel(1):     *)
(*               "ok" (Call::reply), "ecanceled" (Call::drop),             *)
(*               "error" (remoteObjectProxyWithErrorHandler block),        *)
(*               "timeout" (recv_timeout)                                  *)
(*   valid       NSXPCConnection not yet invalidated                       *)
(*                                                                         *)
(* Every critical section of Queue::push/receive/close/discard runs to     *)
(* completion without blocking, so each one is a single atomic action and  *)
(* the Mutex itself is implicit. Condvar::notify_one moves one arbitrary   *)
(* parked waiter back to "check"; notify_all moves all of them.            *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Callers, Receivers, Cap, UseTimeout,
          MaxSpurious, MaxDecodeErrors, MaxShutdowns

NoCall == "none"

VARIABLES items, closed, rpc, serving, cpc, inflight, reply, valid,
          spurious, decodeErrors, shutdowns

vars == <<items, closed, rpc, serving, cpc, inflight, reply, valid,
          spurious, decodeErrors, shutdowns>>

Waiting == {r \in Receivers : rpc[r] = "wait"}

TypeOK ==
  /\ items \in Seq(Callers) /\ closed \in BOOLEAN /\ valid \in BOOLEAN
  /\ rpc \in [Receivers -> {"check", "wait", "serve", "eof", "done"}]
  /\ serving \in [Receivers -> Callers \cup {NoCall}]
  /\ cpc \in [Callers -> {"idle", "waiting", "done"}]
  /\ inflight \subseteq Callers
  /\ reply \in [Callers -> {"none", "ok", "ecanceled", "error", "timeout"}]

Init ==
  /\ items = <<>> /\ closed = FALSE /\ valid = TRUE
  /\ rpc = [r \in Receivers |-> "check"]
  /\ serving = [r \in Receivers |-> NoCall]
  /\ cpc = [a \in Callers |-> "idle"]
  /\ inflight = {}
  /\ reply = [a \in Callers |-> "none"]
  /\ spurious = 0 /\ decodeErrors = 0 /\ shutdowns = 0

\* Condvar::notify_one / notify_all applied to the post-state of rpc.
NotifyOne(r0) ==
  IF Waiting = {} THEN rpc' = r0
  ELSE \E w \in Waiting : rpc' = [r0 EXCEPT ![w] = "check"]
NotifyAll(r0) == rpc' = [r \in Receivers |-> IF rpc[r] = "wait" THEN "check" ELSE r0[r]]

\* sync_channel(1).try_send: only the first result is observed.
Answer(a, v) == reply' = IF reply[a] = "none" /\ cpc[a] = "waiting"
                         THEN [reply EXCEPT ![a] = v] ELSE reply

\* Connection::call: polymer_call on the proxy, then recv_timeout.
Call(a) ==
  /\ cpc[a] = "idle"
  /\ cpc' = [cpc EXCEPT ![a] = "waiting"]
  /\ inflight' = inflight \cup {a}
  /\ UNCHANGED <<items, closed, rpc, serving, reply, valid, spurious, decodeErrors, shutdowns>>

\* Receiver::call -> Queue::push; a rejected push drops the Call (ECANCELED).
ReceiverCall(a) ==
  /\ a \in inflight /\ valid
  /\ inflight' = inflight \ {a}
  /\ IF closed \/ Len(items) >= Cap
       THEN /\ Answer(a, "ecanceled")
            /\ UNCHANGED <<items, rpc>>
       ELSE /\ items' = Append(items, a)
            /\ NotifyOne(rpc)
            /\ UNCHANGED reply
  /\ UNCHANGED <<closed, serving, cpc, valid, spurious, decodeErrors, shutdowns>>

\* Queue::receive, one pass of its loop under the mutex.
Receive(r) ==
  /\ rpc[r] = "check"
  /\ IF Len(items) > 0
       THEN /\ serving' = [serving EXCEPT ![r] = Head(items)]
            /\ items' = Tail(items)
            /\ rpc' = [rpc EXCEPT ![r] = "serve"]
       ELSE /\ rpc' = [rpc EXCEPT ![r] = IF closed THEN "eof" ELSE "wait"]
            /\ UNCHANGED <<serving, items>>
  /\ UNCHANGED <<closed, cpc, inflight, reply, valid, spurious, decodeErrors, shutdowns>>

SpuriousWakeup(r) ==
  /\ rpc[r] = "wait" /\ spurious < MaxSpurious
  /\ rpc' = [rpc EXCEPT ![r] = "check"]
  /\ spurious' = spurious + 1
  /\ UNCHANGED <<items, closed, serving, cpc, inflight, reply, valid, decodeErrors, shutdowns>>

\* Call::reply; NSXPC drops a reply on an invalidated connection.
Reply(r) ==
  /\ rpc[r] = "serve"
  /\ IF valid THEN Answer(serving[r], "ok") ELSE UNCHANGED reply
  /\ rpc' = [rpc EXCEPT ![r] = "check"]
  /\ serving' = [serving EXCEPT ![r] = NoCall]
  /\ UNCHANGED <<items, closed, cpc, inflight, valid, spurious, decodeErrors, shutdowns>>

\* Session::serve: Frame::decode(&call.data)? drops the Call and returns.
DecodeError(r) ==
  /\ rpc[r] = "serve" /\ decodeErrors < MaxDecodeErrors
  /\ IF valid THEN Answer(serving[r], "ecanceled") ELSE UNCHANGED reply
  /\ rpc' = [rpc EXCEPT ![r] = "eof"]
  /\ serving' = [serving EXCEPT ![r] = NoCall]
  /\ decodeErrors' = decodeErrors + 1
  /\ UNCHANGED <<items, closed, cpc, inflight, valid, spurious, shutdowns>>

\* Server worker after serve returns: Connection::close (invalidate + Queue::close).
ConnectionClose(r) ==
  /\ rpc[r] = "eof"
  /\ valid' = FALSE /\ closed' = TRUE
  /\ NotifyAll([rpc EXCEPT ![r] = "done"])
  /\ UNCHANGED <<items, serving, cpc, inflight, reply, spurious, decodeErrors, shutdowns>>

\* Server::drop -> Connection::close from another thread.
ServerDrop ==
  /\ shutdowns < MaxShutdowns
  /\ shutdowns' = shutdowns + 1
  /\ valid' = FALSE /\ closed' = TRUE
  /\ NotifyAll(rpc)
  /\ UNCHANGED <<items, serving, cpc, inflight, reply, spurious, decodeErrors>>

\* Client side closes or crashes: the server's NSXPCConnection is invalidated.
PeerInvalidate ==
  /\ shutdowns < MaxShutdowns /\ valid
  /\ shutdowns' = shutdowns + 1
  /\ valid' = FALSE
  /\ UNCHANGED <<items, closed, rpc, serving, cpc, inflight, reply, spurious, decodeErrors>>

\* Listener::close style shutdown: Queue::discard drops every queued Call.
Discard ==
  /\ shutdowns < MaxShutdowns
  /\ shutdowns' = shutdowns + 1
  /\ closed' = TRUE /\ items' = <<>>
  /\ reply' = [a \in Callers |->
                 IF a \in {items[i] : i \in 1..Len(items)} /\ reply[a] = "none" /\ valid
                 THEN "ecanceled" ELSE reply[a]]
  /\ NotifyAll(rpc)
  /\ UNCHANGED <<serving, cpc, inflight, valid, spurious, decodeErrors>>

\* Connection::attach: invalidation / interruption handler -> Incoming::close.
InvalidationHandler ==
  /\ ~valid /\ ~closed
  /\ closed' = TRUE
  /\ NotifyAll(rpc)
  /\ UNCHANGED <<items, serving, cpc, inflight, reply, valid, spurious, decodeErrors, shutdowns>>

\* remoteObjectProxyWithErrorHandler: fires for a call with no reply once invalid.
ErrorHandler(a) ==
  /\ ~valid /\ cpc[a] = "waiting" /\ reply[a] = "none"
  /\ reply' = [reply EXCEPT ![a] = "error"]
  /\ inflight' = inflight \ {a}
  /\ UNCHANGED <<items, closed, rpc, serving, cpc, valid, spurious, decodeErrors, shutdowns>>

Timeout(a) ==
  /\ UseTimeout /\ cpc[a] = "waiting" /\ reply[a] = "none"
  /\ reply' = [reply EXCEPT ![a] = "timeout"]
  /\ UNCHANGED <<items, closed, rpc, serving, cpc, inflight, valid, spurious, decodeErrors, shutdowns>>

\* Connection::call returns the first result received.
CallReturns(a) ==
  /\ cpc[a] = "waiting" /\ reply[a] # "none"
  /\ cpc' = [cpc EXCEPT ![a] = "done"]
  /\ UNCHANGED <<items, closed, rpc, serving, inflight, reply, valid, spurious, decodeErrors, shutdowns>>

Finished ==
  /\ \A a \in Callers : cpc[a] = "done"
  /\ UNCHANGED vars

Next ==
  \/ \E a \in Callers : Call(a) \/ ReceiverCall(a) \/ ErrorHandler(a) \/ Timeout(a) \/ CallReturns(a)
  \/ \E r \in Receivers : Receive(r) \/ SpuriousWakeup(r) \/ Reply(r) \/ DecodeError(r) \/ ConnectionClose(r)
  \/ ServerDrop \/ PeerInvalidate \/ Discard \/ InvalidationHandler
  \/ Finished

Fairness ==
  /\ \A a \in Callers : WF_vars(Call(a)) /\ WF_vars(ReceiverCall(a))
                        /\ WF_vars(ErrorHandler(a)) /\ WF_vars(CallReturns(a))
  /\ \A r \in Receivers : WF_vars(Receive(r)) /\ WF_vars(Reply(r)) /\ WF_vars(ConnectionClose(r))
  /\ WF_vars(InvalidationHandler)

Spec == Init /\ [][Next]_vars /\ Fairness

QueueBounded == Len(items) <= Cap

\* A queued call always has a receiver that is not parked (no lost notify_one).
NoStrandedCall == Len(items) > 0 => \E r \in Receivers : rpc[r] # "wait"

CallCompletes == \A a \in Callers : cpc[a] = "waiting" ~> cpc[a] = "done"
QueuedCallDrained == Len(items) > 0 ~> Len(items) = 0
CloseUnblocksReceivers == closed ~> \A r \in Receivers : rpc[r] = "done"
=============================================================================
