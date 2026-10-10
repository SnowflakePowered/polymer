-------------------------- MODULE BluetoothManager --------------------------
(***************************************************************************)
(* T10: Bluetooth manager sessions, connect/reconnect and BR/EDR pairing.  *)
(*                                                                         *)
(* Rust mapping (src/manager/, src/transport/bredr/):                      *)
(*   sessions[a]    BluetoothActor::sessions (session_id, 0 = absent)      *)
(*   shandle[a]     the session's BrEdrHid::handle                         *)
(*   op[a]          one in-flight connect future: BluetoothActor::connect  *)
(*                  (outbound) or accept_inbound (inbound), wrapped in     *)
(*                  Abortable + timeout; op.id is connect_tasks[a].id,     *)
(*                  op.h the transport's ACL handle                        *)
(*   connecting     BluetoothActor::connecting_transports                  *)
(*   wake[a]        BluetoothActor::wake_tasks (arm_ble_wake); "fired" =   *)
(*                  task sent WakeDiscovered but handle still in the map   *)
(*   mailbox        do_send'd SessionEnded (spawn_reports) and             *)
(*                  WakeDiscovered (arm_ble_wake) messages                 *)
(*   store[a]       ControllerStore record + link key (durable);          *)
(*                  ControllerLinkKeyStore::register / put / remove        *)
(*   queue          HCI events the PairingAgent consumes (host.events())   *)
(*   phys[a]        physical ACL / GATT link of device a                   *)
(*   handle[a]      controller connection handle of a's live ACL (0 none). *)
(*                  The controller frees it on disconnection and may hand  *)
(*                  it to a later ACL only with ReuseHandles; otherwise    *)
(*                  every ACL gets a handle never used before (used).      *)
(*   teardown       HciHost::disconnect_link calls in flight.  checked =   *)
(*                  the registry check passed and the Disconnect is on its *)
(*                  way to the controller; stale = the ACL that handle     *)
(*                  named is gone.                                         *)
(*   missed         transports whose report task lost the link's           *)
(*                  DisconnectionComplete to RecvError::Lagged; the task   *)
(*                  re-checks the HciHost ACL registry on Lagged           *)
(*                  (LagRecheck)                                           *)
(*   nextId         next_session_id                                        *)
(*   overwrote      ghost: finish_connection inserted over a live session  *)
(*   connectedNoKey ghost: ManagerEvent::Connected for a BR/EDR device     *)
(*                  whose link key was not yet in the store                *)
(*   wrongKill      ghost: a stale Disconnect hit another ACL that reused  *)
(*                  the handle                                             *)
(*                                                                         *)
(* Connect future stages (connection/transport.rs, bredr/hid.rs):          *)
(*   accept  accept_inbound: persisted-record check, accept_connection     *)
(*   acl     keys.register + BrEdrHid::connect_acl / BleGatt::connect      *)
(*   sdp     (SdpGap) generic BR/EDR identification: the ACL is open and   *)
(*           sdp::query_identity runs; connect_acl already subscribed the  *)
(*           receiver authenticate() later uses.  The peer may start       *)
(*           authentication itself (op.peer).                              *)
(*   auth    HciHost::authenticate / wait_for_encryption.  It captures a   *)
(*           LinkKeyNotification for the device (op.newKey), ends early on *)
(*           DisconnectionComplete, and BrEdrHid persists the captured key *)
(*           before HID setup continues.  On a link the peer already       *)
(*           authenticated it completes at once.                           *)
(*   hid     L2CAP HID channels / GATT bond                                *)
(*   sync    ControllerSession::synchronize                                *)
(*   synced  synchronize returned Ok; keys.register, then the future       *)
(*           returns into finish_connection, which subscribes to          *)
(*           disconnections(), checks state(), inserts the session and     *)
(*           spawns report forwarding in one actor step                    *)
(*                                                                         *)
(* Pairing agent events: lkreq = LinkKeyRequest, pin = PinCodeRequest /    *)
(* IoCapabilityRequest / UserConfirmationRequest (each needs a reply),     *)
(* lknotify = LinkKeyNotification.  Handling an event models the moment    *)
(* the controller emits it.  The controller sends LinkKeyNotification      *)
(* before AuthenticationComplete, so authenticate() sees it on its own     *)
(* receiver in order if it already subscribed.  The agent logs reply and   *)
(* store errors and keeps running.                                         *)
(*                                                                         *)
(* Disconnects: BrEdrHid records the HciHost ACL identity for its handle   *)
(* and disconnects through disconnect_link, which sends nothing once that  *)
(* ACL is gone.  The check and the controller's acknowledgement are        *)
(* separate steps.  With ConnectionGate, no ACL is created between them:   *)
(* every BR/EDR ACL needs a host Create/Accept Connection (event filters   *)
(* are cleared, so there is no auto-accept), and those wait for the gate.  *)
(*                                                                         *)
(* Abstracted: scanning/discovery (devices are always discovered),         *)
(* suspended scans, player index, external ownership, SendRaw and other    *)
(* atomic session handlers (take + reinsert within one atomic step),       *)
(* Shutdown, wake_gate, process crash, HCI host closure.  BLE bonding is   *)
(* the OS's.  Inbound attach without host security (DS3, bonded Wii        *)
(* Remote) is modelled like the authenticated inbound paths.               *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Addrs, Radio, Handles, MaxIds, MaxDrops, MaxFailures, MaxForgets,
          MaxLags, SdpGap, ReuseHandles, ConnectionGate

ASSUME Radio \in [Addrs -> {"ble", "bredr"}]
ASSUME SdpGap \in BOOLEAN /\ ReuseHandles \in BOOLEAN /\ ConnectionGate \in BOOLEAN

NoOp == [id |-> 0, kind |-> "none", stage |-> "none", authOk |-> FALSE,
         newKey |-> FALSE, peer |-> "none", h |-> 0, reconnect |-> FALSE]
NoRec == [rec |-> FALSE, key |-> FALSE]

VARIABLES sessions, shandle, op, connecting, phys, handle, used, mailbox, wake, store,
          queue, teardown, missed, nextId, drops, lags, failures, forgets,
          overwrote, connectedNoKey, wrongKill

link == <<phys, handle, teardown, used>>
counters == <<nextId, drops, lags, failures, forgets>>

vars == <<sessions, shandle, op, connecting, link, mailbox, wake, store, queue, missed,
          counters, overwrote, connectedNoKey, wrongKill>>

Stages == {"none", "accept", "acl", "sdp", "auth", "hid", "sync", "synced"}

TypeOK ==
  /\ sessions \in [Addrs -> 0..MaxIds]
  /\ shandle \in [Addrs -> Handles \cup {0}]
  /\ op \in [Addrs -> [id : 0..MaxIds, kind : {"none", "out", "in"}, stage : Stages,
                       authOk : BOOLEAN, newKey : BOOLEAN,
                       peer : {"none", "started", "done"}, h : Handles \cup {0},
                       reconnect : BOOLEAN]]
  /\ connecting \subseteq {"ble", "bredr"}
  /\ phys \in [Addrs -> {"down", "up"}]
  /\ handle \in [Addrs -> Handles \cup {0}]
  /\ wake \in [Addrs -> {"none", "armed", "fired"}]
  /\ store \in [Addrs -> [rec : BOOLEAN, key : BOOLEAN]]
  /\ teardown \subseteq [h : Handles, stale : BOOLEAN, checked : BOOLEAN]
  /\ used \subseteq Handles
  /\ missed \subseteq Addrs

Init ==
  /\ sessions = [a \in Addrs |-> 0]
  /\ shandle = [a \in Addrs |-> 0]
  /\ op = [a \in Addrs |-> NoOp]
  /\ connecting = {}
  /\ phys = [a \in Addrs |-> "down"]
  /\ handle = [a \in Addrs |-> 0]
  /\ used = {}
  /\ mailbox = {}
  /\ wake = [a \in Addrs |-> "none"]
  /\ store = [a \in Addrs |-> NoRec]
  /\ queue = <<>>
  /\ teardown = {}
  /\ missed = {}
  /\ nextId = 1
  /\ drops = 0 /\ lags = 0 /\ failures = 0 /\ forgets = 0
  /\ overwrote = FALSE /\ connectedNoKey = FALSE /\ wrongKill = FALSE

----------------------------------------------------------------------------
\* Links and handles

HeldHandles == {handle[b] : b \in Addrs} \ {0}

FreeHandles ==
  IF ReuseHandles THEN Handles \ HeldHandles ELSE Handles \ used

LinkHandles(a) == IF Radio[a] = "bredr" THEN FreeHandles ELSE {0}

Staled(t, h) == {IF e.h = h THEN [e EXCEPT !.stale = TRUE] ELSE e : e \in t}

\* HciHost::connection_gate: a Disconnect that passed its registry check
\* holds the gate until the controller acknowledges it, and ACL creation
\* waits for the gate.
GateOpen == ~ConnectionGate \/ \A e \in teardown : ~e.checked

\* a's live ACL goes down: the controller frees its handle.
LinkDown(a) ==
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ handle' = [handle EXCEPT ![a] = 0]
  /\ teardown' = IF handle[a] # 0 THEN Staled(teardown, handle[a]) ELSE teardown
  /\ UNCHANGED used

Live(a, h) == h # 0 /\ handle[a] = h

\* An awaited BrEdrHid / BleGatt disconnect using transport handle h.
\* HciHost::disconnect_link checks the registry now; the Disconnect it then
\* sends reaches the controller later (TeardownSend).
SyncDisconnect(a, h) ==
  IF Radio[a] = "ble"
    THEN IF phys[a] = "up"
           THEN /\ phys' = [phys EXCEPT ![a] = "down"]
                /\ UNCHANGED <<handle, teardown, used>>
           ELSE UNCHANGED link
  ELSE IF Live(a, h)
    THEN /\ teardown' = teardown \cup {[h |-> h, stale |-> FALSE, checked |-> TRUE]}
         /\ UNCHANGED <<phys, handle, used>>
  ELSE UNCHANGED link

\* BrEdrHid::drop spawns host.disconnect_link, which checks the registry
\* when it runs (TeardownCheck); BleGatt releases the peripheral at once.
AsyncDisconnect(a, h) ==
  IF Radio[a] = "ble"
    THEN /\ phys' = [phys EXCEPT ![a] = "down"]
         /\ UNCHANGED <<handle, teardown, used>>
  ELSE IF h # 0
    THEN /\ teardown' = teardown \cup {[h |-> h, stale |-> ~Live(a, h), checked |-> FALSE]}
         /\ UNCHANGED <<phys, handle, used>>
  ELSE UNCHANGED link

----------------------------------------------------------------------------
\* Connect attempts

ArmWakeF(w, a) == IF Radio[a] = "ble" /\ w[a] = "none" THEN [w EXCEPT ![a] = "armed"] ELSE w

ArmWake(a) == wake' = ArmWakeF(wake, a)

SetStage(a, s) == op' = [op EXCEPT ![a].stage = s]

NewOp(kind, stage, reconnect) ==
  [id |-> nextId, kind |-> kind, stage |-> stage, authOk |-> FALSE,
   newKey |-> FALSE, peer |-> "none", h |-> 0, reconnect |-> reconnect]

\* begin_connect: early Ok if a session exists, error if busy (re-arming a
\* reconnect), otherwise start the connect future.
BeginConnect(a, reconnect, w) ==
  IF sessions[a] # 0 THEN /\ wake' = w /\ UNCHANGED <<op, connecting, nextId>>
  ELSE IF op[a] # NoOp \/ Radio[a] \in connecting
    THEN /\ wake' = IF reconnect THEN ArmWakeF(w, a) ELSE w
         /\ UNCHANGED <<op, connecting, nextId>>
  ELSE /\ op' = [op EXCEPT ![a] = NewOp("out", "acl", reconnect)]
       /\ connecting' = connecting \cup {Radio[a]}
       /\ nextId' = nextId + 1
       /\ wake' = w

\* finish_connection with the connect_tasks id matching.
FinishConnection(a, ok) ==
  /\ op' = [op EXCEPT ![a] = NoOp]
  /\ connecting' = connecting \ {Radio[a]}
  /\ IF ok
       THEN /\ sessions' = [sessions EXCEPT ![a] = op[a].id]
            /\ shandle' = [shandle EXCEPT ![a] = op[a].h]
            /\ overwrote' = (overwrote \/ sessions[a] # 0)
            /\ connectedNoKey' = (connectedNoKey \/ (Radio[a] = "bredr" /\ ~store[a].key))
            /\ UNCHANGED wake
       ELSE /\ IF op[a].reconnect THEN ArmWake(a) ELSE UNCHANGED wake
            /\ UNCHANGED <<sessions, shandle, overwrote, connectedNoKey>>

\* A failed attempt whose transport disconnects its own handle on the way out.
Abandon(a) ==
  /\ FinishConnection(a, FALSE)
  /\ SyncDisconnect(a, op[a].h)

\* Connect handler (user Connect or scan_finished's auto Connect).
Connect(a) ==
  /\ nextId <= MaxIds
  /\ BeginConnect(a, FALSE, wake)
  /\ UNCHANGED <<sessions, shandle, link, mailbox, store, queue, missed, drops, lags,
                 failures, forgets, overwrote, connectedNoKey, wrongKill>>

\* HciLifecycle(ConnectionRequest): device-initiated BR/EDR reconnect.
HciLifecycle(a) ==
  /\ Radio[a] = "bredr" /\ phys[a] = "down" /\ nextId <= MaxIds
  /\ ~(sessions[a] # 0 \/ op[a] # NoOp \/ "bredr" \in connecting)
  /\ op' = [op EXCEPT ![a] = NewOp("in", "accept", FALSE)]
  /\ connecting' = connecting \cup {"bredr"}
  /\ nextId' = nextId + 1
  /\ UNCHANGED <<sessions, shandle, link, mailbox, wake, store, queue, missed, drops,
                 lags, failures, forgets, overwrote, connectedNoKey, wrongKill>>

\* accept_inbound: requires a persisted auto-reconnect record, then accepts.
AcceptInbound(a) ==
  /\ op[a].stage = "accept"
  /\ IF store[a].rec
       THEN \E h \in FreeHandles :
              /\ GateOpen
              /\ phys' = [phys EXCEPT ![a] = "up"]
              /\ handle' = [handle EXCEPT ![a] = h]
              /\ used' = used \cup {h}
              /\ op' = [op EXCEPT ![a].stage = "auth", ![a].h = h]
              /\ queue' = Append(queue, [kind |-> "lkreq", addr |-> a])
              /\ missed' = missed \ {a}
              /\ UNCHANGED <<teardown, connecting, wake, sessions, shandle, overwrote,
                             connectedNoKey>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED <<link, queue, missed>>
  /\ UNCHANGED <<mailbox, store, counters, wrongKill>>

\* connect(): keys.register, then connect_acl (+ authenticate) or BleGatt::connect.
\* With SdpGap a generic BR/EDR device is identified over SDP first.
Acl(a) ==
  /\ op[a].stage = "acl"
  /\ IF phys[a] = "down"
       THEN \E h \in LinkHandles(a), sdp \in IF SdpGap THEN BOOLEAN ELSE {FALSE} :
              /\ Radio[a] = "ble" \/ GateOpen
              /\ phys' = [phys EXCEPT ![a] = "up"]
              /\ handle' = [handle EXCEPT ![a] = h]
              /\ used' = (used \cup {h}) \ {0}
              /\ store' = [store EXCEPT ![a].rec = TRUE]
              /\ missed' = missed \ {a}
              /\ op' = [op EXCEPT ![a].h = h,
                                  ![a].stage = IF Radio[a] = "ble" THEN "hid"
                                               ELSE IF sdp THEN "sdp" ELSE "auth"]
              /\ queue' = IF Radio[a] = "bredr" /\ ~sdp
                            THEN Append(queue, [kind |-> "lkreq", addr |-> a])
                            ELSE queue
              /\ UNCHANGED <<teardown, connecting, wake, sessions, shandle, overwrote,
                             connectedNoKey>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED <<link, store, queue, missed>>
  /\ UNCHANGED <<mailbox, counters, wrongKill>>

\* The peer authenticates on its own while SDP identification runs.
SdpPeerAuthenticates(a) ==
  /\ op[a].stage = "sdp" /\ op[a].peer = "none" /\ phys[a] = "up"
  /\ op' = [op EXCEPT ![a].peer = "started"]
  /\ queue' = Append(queue, [kind |-> "lkreq", addr |-> a])
  /\ UNCHANGED <<sessions, shandle, connecting, link, mailbox, wake, store, missed,
                 counters, overwrote, connectedNoKey, wrongKill>>

\* SDP identification finished; authenticate() subscribes and requests
\* authentication, which completes at once if the peer already did it.
SdpDone(a) ==
  /\ op[a].stage = "sdp"
  /\ IF phys[a] = "up"
       THEN /\ op' = [op EXCEPT ![a].stage = "auth",
                                ![a].authOk = (op[a].peer = "done")]
            /\ queue' = IF op[a].peer = "none"
                          THEN Append(queue, [kind |-> "lkreq", addr |-> a])
                          ELSE queue
            /\ UNCHANGED <<link, connecting, wake, sessions, shandle, overwrote,
                           connectedNoKey>>
       ELSE /\ Abandon(a)
            /\ UNCHANGED queue
  /\ UNCHANGED <<mailbox, store, missed, counters, wrongKill>>

\* BrEdrHid::persist: the key captured by authenticate() is written before
\* HID setup.  A failed write abandons the attempt and disconnects.
PersistKey(a) ==
  /\ op[a].stage = "auth" /\ op[a].authOk /\ op[a].newKey
  /\ \/ /\ store' = [store EXCEPT ![a] = [rec |-> TRUE, key |-> TRUE]]
        /\ op' = [op EXCEPT ![a].newKey = FALSE]
        /\ UNCHANGED <<connecting, wake, sessions, shandle, link, failures, overwrote,
                       connectedNoKey>>
     \/ /\ failures < MaxFailures
        /\ failures' = failures + 1
        /\ Abandon(a)
        /\ UNCHANGED store
  /\ UNCHANGED <<mailbox, queue, missed, nextId, drops, lags, forgets, wrongKill>>

\* authenticate() returns on a matching DisconnectionComplete and abandon()
\* disconnects the handle it still holds.
AuthDisconnected(a) ==
  /\ op[a].stage = "auth" /\ phys[a] = "down"
  /\ Abandon(a)
  /\ UNCHANGED <<mailbox, store, queue, missed, counters, wrongKill>>

\* authenticate()'s own 30 s timeout; abandon() disconnects.
AuthTimeout(a) ==
  /\ op[a].stage = "auth" /\ ~op[a].authOk
  /\ Abandon(a)
  /\ UNCHANGED <<mailbox, store, queue, missed, counters, wrongKill>>

\* BrEdrHid channel setup after authenticate() / GATT bond.
OpenHid(a) ==
  /\ \/ op[a].stage = "hid"
     \/ op[a].stage = "auth" /\ op[a].authOk /\ ~op[a].newKey
  /\ IF phys[a] = "up"
       THEN /\ SetStage(a, "sync")
            /\ UNCHANGED <<connecting, wake, sessions, shandle, overwrote, connectedNoKey,
                           link>>
       ELSE Abandon(a)
  /\ UNCHANGED <<mailbox, store, queue, missed, counters, wrongKill>>

\* controller_session.synchronize: Ok, or Err -> transport.disconnect.
Synchronize(a) ==
  /\ op[a].stage = "sync"
  /\ SetStage(a, "synced")
  /\ UNCHANGED <<sessions, shandle, connecting, link, mailbox, wake, store, queue, missed,
                 counters, overwrote, connectedNoKey, wrongKill>>

SynchronizeFails(a) ==
  /\ op[a].stage = "sync" /\ phys[a] = "down"
  /\ Abandon(a)
  /\ UNCHANGED <<mailbox, store, queue, missed, counters, wrongKill>>

\* keys.register, then finish_connection: subscribe, check state(), insert
\* and spawn report forwarding, or drop a transport whose link already ended
\* (its state is DISCONNECTED, so Drop sends nothing).  A transport that
\* missed the drop still reports CONNECTED.
SpawnReports(a) ==
  /\ op[a].stage = "synced"
  /\ store' = [store EXCEPT ![a].rec = TRUE]
  /\ FinishConnection(a, phys[a] = "up" \/ a \in missed)
  /\ UNCHANGED <<link, mailbox, queue, missed, counters, wrongKill>>

\* time::timeout(CONNECTION_ATTEMPT_TIMEOUT): the future is dropped.
Timeout(a) ==
  /\ op[a].stage \in {"accept", "acl", "sdp", "auth", "hid", "sync"}
  /\ FinishConnection(a, FALSE)
  /\ AsyncDisconnect(a, op[a].h)
  /\ UNCHANGED <<mailbox, store, queue, missed, counters, wrongKill>>

----------------------------------------------------------------------------
\* Pairing agent

\* PairingAgent::handle for the head of host.events().  Replies to a link
\* that is already gone fail and are logged.  A pairing exchange produces a
\* LinkKeyNotification that the attempt captures on the receiver
\* connect_acl subscribed before the ACL existed (stages sdp and auth).
PairingAgentHandle ==
  /\ queue # <<>>
  /\ LET e == Head(queue)
         a == e.addr
         authing == op[a].stage = "auth"
         peerDone == IF op[a].stage = "sdp" THEN [op EXCEPT ![a].peer = "done"] ELSE op
         paired == IF op[a].stage = "sdp"
                     THEN [op EXCEPT ![a].peer = "done", ![a].newKey = TRUE]
                     ELSE op
     IN
     CASE e.kind \in {"lkreq", "pin"} /\ phys[a] = "down" ->
            /\ queue' = Tail(queue)
            /\ UNCHANGED <<op, store>>
       [] e.kind = "lkreq" /\ store[a].key ->
            /\ op' = IF authing THEN [op EXCEPT ![a].authOk = TRUE] ELSE peerDone
            /\ queue' = Tail(queue)
            /\ UNCHANGED store
       [] e.kind = "lkreq" ->
            /\ queue' = Append(Tail(queue), [kind |-> "pin", addr |-> a])
            /\ UNCHANGED <<op, store>>
       [] e.kind = "pin" ->
            /\ queue' = Append(Tail(queue), [kind |-> "lknotify", addr |-> a])
            /\ op' = IF authing
                       THEN [op EXCEPT ![a].authOk = TRUE, ![a].newKey = TRUE]
                       ELSE paired
            /\ UNCHANGED store
       [] e.kind = "lknotify" ->
            /\ store' = [store EXCEPT ![a] = [rec |-> TRUE, key |-> TRUE]]
            /\ queue' = Tail(queue)
            /\ UNCHANGED op
  /\ UNCHANGED <<sessions, shandle, connecting, link, mailbox, wake, missed, counters,
                 overwrote, connectedNoKey, wrongKill>>

\* The agent's own ControllerLinkKeyStore::put fails; it logs and continues.
LinkKeyPutFails ==
  /\ queue # <<>> /\ Head(queue).kind = "lknotify"
  /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ queue' = Tail(queue)
  /\ UNCHANGED <<sessions, shandle, op, connecting, link, mailbox, wake, store, missed,
                 nextId, drops, lags, forgets, overwrote, connectedNoKey, wrongKill>>

----------------------------------------------------------------------------
\* Link events and actor handlers

EndedMsg(a) == [kind |-> "ended", addr |-> a, id |-> sessions[a]]

\* Device or radio drops the link.  The transport report task stores
\* DISCONNECTED and broadcasts on `disconnects`; a live session's report
\* forwarding turns that into SessionEnded.  With MaxLags the report task
\* can lose the DisconnectionComplete to Lagged.
PhysDrop(a) ==
  /\ phys[a] = "up" /\ drops < MaxDrops
  /\ drops' = drops + 1
  /\ LinkDown(a)
  /\ \/ /\ mailbox' = IF sessions[a] # 0 THEN mailbox \cup {EndedMsg(a)} ELSE mailbox
        /\ UNCHANGED <<missed, lags>>
     \/ /\ lags < MaxLags
        /\ sessions[a] # 0 \/ op[a].stage \in {"sync", "synced"}
        /\ lags' = lags + 1
        /\ missed' = missed \cup {a}
        /\ UNCHANGED mailbox
  /\ UNCHANGED <<sessions, shandle, op, connecting, wake, store, queue, nextId, failures,
                 forgets, overwrote, connectedNoKey, wrongKill>>

\* The report task's next recv returns Lagged; it re-checks the ACL registry,
\* finds its ACL gone, stores DISCONNECTED and ends the session.
LagRecheck(a) ==
  /\ a \in missed
  /\ missed' = missed \ {a}
  /\ mailbox' = IF sessions[a] # 0 THEN mailbox \cup {EndedMsg(a)} ELSE mailbox
  /\ UNCHANGED <<sessions, shandle, op, connecting, link, wake, store, queue, counters,
                 overwrote, connectedNoKey, wrongKill>>

\* The task spawned by BrEdrHid::drop takes the gate and checks the ACL
\* registry; a gone ACL needs no Disconnect.
TeardownCheck(e) ==
  /\ e \in teardown /\ ~e.checked
  /\ teardown' = IF e.stale THEN teardown \ {e}
                 ELSE (teardown \ {e}) \cup {[e EXCEPT !.checked = TRUE]}
  /\ UNCHANGED <<phys, handle, used, sessions, shandle, op, connecting, mailbox, wake,
                 store, queue, missed, counters, overwrote, connectedNoKey, wrongKill>>

\* The controller acknowledges a checked Disconnect and disconnects whatever
\* ACL holds the handle now.  If the intended ACL dropped after the check
\* and another ACL took the handle, that ACL is lost (wrongKill).
TeardownSend(e) ==
  /\ e \in teardown /\ e.checked
  /\ IF \E b \in Addrs : handle[b] = e.h
       THEN \E b \in Addrs :
              /\ handle[b] = e.h
              /\ phys' = [phys EXCEPT ![b] = "down"]
              /\ handle' = [handle EXCEPT ![b] = 0]
              /\ teardown' = Staled(teardown \ {e}, e.h)
              /\ UNCHANGED used
              /\ wrongKill' = (wrongKill \/ e.stale)
              /\ mailbox' = IF sessions[b] # 0 /\ b \notin missed
                              THEN mailbox \cup {EndedMsg(b)} ELSE mailbox
       ELSE /\ teardown' = teardown \ {e}
            /\ UNCHANGED <<phys, handle, used, wrongKill, mailbox>>
  /\ UNCHANGED <<sessions, shandle, op, connecting, wake, store, queue, missed, counters,
                 overwrote, connectedNoKey>>

TeardownStep == \E e \in teardown : TeardownCheck(e) \/ TeardownSend(e)

\* Handler<SessionEnded>.
SessionEnded(m) ==
  /\ m \in mailbox /\ m.kind = "ended"
  /\ mailbox' = mailbox \ {m}
  /\ IF sessions[m.addr] = m.id
       THEN /\ sessions' = [sessions EXCEPT ![m.addr] = 0]
            /\ shandle' = [shandle EXCEPT ![m.addr] = 0]
            /\ IF store[m.addr].rec THEN ArmWake(m.addr) ELSE UNCHANGED wake
       ELSE UNCHANGED <<sessions, shandle, wake>>
  /\ UNCHANGED <<op, connecting, link, store, queue, missed, counters, overwrote,
                 connectedNoKey, wrongKill>>

\* arm_ble_wake task finds the advertisement and do_sends WakeDiscovered.
WakeScan(a) ==
  /\ wake[a] = "armed" /\ phys[a] = "down"
  /\ wake' = [wake EXCEPT ![a] = "fired"]
  /\ mailbox' = mailbox \cup {[kind |-> "wake", addr |-> a, id |-> 0]}
  /\ UNCHANGED <<sessions, shandle, op, connecting, link, store, queue, missed, counters,
                 overwrote, connectedNoKey, wrongKill>>

\* Handler<WakeDiscovered>: stale task ids are ignored; otherwise begin_connect.
\* With session ids exhausted the reconnect is dropped (model bound).
WakeDiscovered(m) ==
  /\ m \in mailbox /\ m.kind = "wake"
  /\ mailbox' = mailbox \ {m}
  /\ IF wake[m.addr] = "fired" /\ nextId <= MaxIds
       THEN BeginConnect(m.addr, TRUE, [wake EXCEPT ![m.addr] = "none"])
       ELSE IF wake[m.addr] = "fired"
         THEN /\ wake' = [wake EXCEPT ![m.addr] = "none"]
              /\ UNCHANGED <<op, connecting, nextId>>
         ELSE UNCHANGED <<op, connecting, nextId, wake>>
  /\ UNCHANGED <<sessions, shandle, link, store, queue, missed, drops, lags, failures,
                 forgets, overwrote, connectedNoKey, wrongKill>>

\* disconnect_session (Disconnect / Sleep).  Sleep re-arms the BLE wake.
DisconnectSession(a, sleep) ==
  /\ sessions[a] # 0
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ shandle' = [shandle EXCEPT ![a] = 0]
  /\ SyncDisconnect(a, shandle[a])
  /\ IF sleep /\ store[a].rec THEN ArmWake(a) ELSE UNCHANGED wake
  /\ UNCHANGED <<op, connecting, mailbox, store, queue, missed, counters, overwrote,
                 connectedNoKey, wrongKill>>

\* Handler<Forget>: abort the connect future (BrEdrHid::drop spawns its
\* disconnect), drop the wake task, disconnect the session, pairing.forget +
\* store.remove.
Forget(a) ==
  /\ forgets < MaxForgets
  /\ forgets' = forgets + 1
  /\ op' = [op EXCEPT ![a] = NoOp]
  /\ connecting' = IF op[a] # NoOp THEN connecting \ {Radio[a]} ELSE connecting
  /\ wake' = [wake EXCEPT ![a] = "none"]
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ shandle' = [shandle EXCEPT ![a] = 0]
  /\ IF sessions[a] # 0 THEN SyncDisconnect(a, shandle[a])
     ELSE IF op[a] # NoOp THEN AsyncDisconnect(a, op[a].h)
     ELSE UNCHANGED link
  /\ store' = [store EXCEPT ![a] = NoRec]
  /\ UNCHANGED <<mailbox, queue, missed, nextId, drops, lags, failures, overwrote,
                 connectedNoKey, wrongKill>>

----------------------------------------------------------------------------

Quiet ==
  /\ \A a \in Addrs : op[a] = NoOp
  /\ mailbox = {}
  /\ teardown = {}
  /\ queue = <<>>
  /\ missed = {}

Finished == Quiet /\ UNCHANGED vars

ConnectProgress(a) ==
  \/ AcceptInbound(a) \/ Acl(a) \/ SdpDone(a) \/ PersistKey(a) \/ AuthDisconnected(a)
  \/ OpenHid(a) \/ Synchronize(a) \/ SynchronizeFails(a) \/ SpawnReports(a)
  \/ AuthTimeout(a)

ActorProgress ==
  \/ \E m \in mailbox : SessionEnded(m) \/ WakeDiscovered(m)

Next ==
  \/ \E a \in Addrs :
       \/ Connect(a) \/ HciLifecycle(a) \/ ConnectProgress(a) \/ PhysDrop(a)
       \/ SdpPeerAuthenticates(a)
       \/ WakeScan(a) \/ DisconnectSession(a, FALSE) \/ DisconnectSession(a, TRUE)
       \/ Timeout(a) \/ Forget(a) \/ LagRecheck(a)
  \/ TeardownStep
  \/ PairingAgentHandle \/ LinkKeyPutFails
  \/ ActorProgress
  \/ Finished

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A a \in Addrs : WF_vars(ConnectProgress(a))
  /\ WF_vars(TeardownStep)
  /\ \A a \in Addrs : WF_vars(LagRecheck(a))
  /\ WF_vars(PairingAgentHandle)
  /\ WF_vars(ActorProgress)

\* finish_connection never replaces a live session in the map.
NoSessionOverwrite == ~overwrote

\* A device address never has both a session and a connect attempt.
OneOwnerPerAddress == \A a \in Addrs : ~(sessions[a] # 0 /\ op[a] # NoOp)

\* connecting_transports is exactly the radios with an attempt in flight.
ConnectingMatchesTasks ==
  connecting = {Radio[a] : a \in {b \in Addrs : op[b] # NoOp}}

\* A BR/EDR device is reported Connected only after its link key is stored.
KeyPersistedBeforeConnected == ~connectedNoKey

\* No physical link is left without a session, attempt or pending teardown.
NoOrphanLink ==
  \A a \in Addrs :
    phys[a] = "up" =>
      \/ sessions[a] # 0 \/ op[a] # NoOp
      \/ \E e \in teardown : e.h = handle[a] /\ ~e.stale

\* A Disconnect only ever reaches the ACL it was meant for.
NoWrongDisconnect == ~wrongKill

\* Every Disconnect handed to HciHost eventually completes.
TeardownCompletes ==
  \A h \in Handles :
    (\E e \in teardown : e.h = h) ~> \A e \in teardown : e.h # h

\* A session whose link went down is eventually removed.
NoStuckSession ==
  \A a \in Addrs : (sessions[a] # 0 /\ phys[a] = "down") ~> sessions[a] = 0

\* Every connect attempt eventually finishes.
ConnectAttemptEnds == \A a \in Addrs : op[a] # NoOp ~> op[a] = NoOp

=============================================================================
