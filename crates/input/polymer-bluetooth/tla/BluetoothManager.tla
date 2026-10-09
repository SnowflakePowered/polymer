-------------------------- MODULE BluetoothManager --------------------------
(***************************************************************************)
(* T10: Bluetooth manager sessions, connect/reconnect and BR/EDR pairing.  *)
(*                                                                         *)
(* Rust mapping (src/manager/, src/transport/bredr/pairing.rs):            *)
(*   sessions[a]    BluetoothActor::sessions (session_id, 0 = absent)      *)
(*   op[a]          one in-flight connect future: BluetoothActor::connect  *)
(*                  (outbound) or accept_inbound (inbound), wrapped in     *)
(*                  Abortable + timeout; op.id is connect_tasks[a].id      *)
(*   connecting     BluetoothActor::connecting_transports                  *)
(*   wake[a]        BluetoothActor::wake_tasks (arm_ble_wake); "fired" =   *)
(*                  task sent WakeDiscovered but handle still in the map   *)
(*   mailbox        do_send'd SessionEnded (spawn_reports) and             *)
(*                  WakeDiscovered (arm_ble_wake) messages                 *)
(*   store[a]       ControllerStore record + link key (durable);          *)
(*                  ControllerLinkKeyStore::register / put / remove        *)
(*   agentAlive     PairingAgent::run task (pairing_task) still running    *)
(*   queue          HCI events the PairingAgent consumes (host.events())   *)
(*   phys[a]        physical ACL / GATT link of device a.  The transport's *)
(*                  retained state() mirrors it once HID setup subscribed  *)
(*                  to HCI events; earlier drops fail channel setup.       *)
(*   teardown       dropped BrEdrHid values whose Drop spawned            *)
(*                  host.disconnect for a link that is still up            *)
(*   nextId         next_session_id                                        *)
(*   overwrote      ghost: finish_connection inserted over a live session  *)
(*   connectedNoKey ghost: ManagerEvent::Connected for a BR/EDR device     *)
(*                  whose link key was not yet in the store                *)
(*                                                                         *)
(* Connect future stages (connection/transport.rs, bredr/hid.rs):          *)
(*   accept  accept_inbound: persisted-record check, accept_connection     *)
(*   acl     keys.register + BrEdrHid::connect_acl / BleGatt::connect      *)
(*   auth    HciHost::authenticate / wait_for_encryption.  It captures a   *)
(*           LinkKeyNotification for the device (op.newKey), ends early on *)
(*           DisconnectionComplete, and BrEdrHid persists the captured key *)
(*           before HID setup continues.                                   *)
(*   hid     L2CAP HID channels / GATT bond                                *)
(*   sync    ControllerSession::synchronize                                *)
(*   synced  synchronize returned Ok; keys.register, then the future       *)
(*           returns into finish_connection, which subscribes to          *)
(*           disconnections(), checks state(), inserts the session and     *)
(*           spawns report forwarding in one actor step                    *)
(*                                                                         *)
(* Pairing agent events: lkreq = LinkKeyRequest, pin = PinCodeRequest /    *)
(* IoCapabilityRequest / UserConfirmationRequest (each needs a reply),     *)
(* lknotify = LinkKeyNotification.  The controller sends                   *)
(* LinkKeyNotification before AuthenticationComplete, so authenticate()    *)
(* sees it on its own receiver in order.  The agent logs reply and store   *)
(* errors and keeps running; it stops only when the HCI host closes,       *)
(* which is not modelled.                                                  *)
(*                                                                         *)
(* Abstracted: scanning/discovery (devices are always discovered),         *)
(* suspended scans, SDP, player index, external ownership, SendRaw and     *)
(* other atomic session handlers (take + reinsert within one atomic step), *)
(* Shutdown, wake_gate, process crash, HCI handle reuse.  BLE bonding is   *)
(* the OS's.  Inbound attach without host security (DS3, bonded Wii        *)
(* Remote) is modelled like the authenticated inbound paths.               *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Addrs, Radio, MaxIds, MaxDrops, MaxFailures, MaxForgets

ASSUME Radio \in [Addrs -> {"ble", "bredr"}]

NoOp == [id |-> 0, kind |-> "none", stage |-> "none", authOk |-> FALSE,
         newKey |-> FALSE, reconnect |-> FALSE]
NoRec == [rec |-> FALSE, key |-> FALSE]

VARIABLES sessions, op, connecting, phys, mailbox, wake, store,
          agentAlive, queue, teardown, nextId, drops, failures, forgets,
          overwrote, connectedNoKey

vars == <<sessions, op, connecting, phys, mailbox, wake, store, agentAlive, queue,
          teardown, nextId, drops, failures, forgets, overwrote, connectedNoKey>>

Stages == {"none", "accept", "acl", "auth", "hid", "sync", "synced"}

TypeOK ==
  /\ sessions \in [Addrs -> 0..MaxIds]
  /\ op \in [Addrs -> [id : 0..MaxIds, kind : {"none", "out", "in"}, stage : Stages,
                       authOk : BOOLEAN, newKey : BOOLEAN, reconnect : BOOLEAN]]
  /\ connecting \subseteq {"ble", "bredr"}
  /\ phys \in [Addrs -> {"down", "up"}]
  /\ wake \in [Addrs -> {"none", "armed", "fired"}]
  /\ store \in [Addrs -> [rec : BOOLEAN, key : BOOLEAN]]
  /\ agentAlive \in BOOLEAN
  /\ teardown \subseteq Addrs

Init ==
  /\ sessions = [a \in Addrs |-> 0]
  /\ op = [a \in Addrs |-> NoOp]
  /\ connecting = {}
  /\ phys = [a \in Addrs |-> "down"]
  /\ mailbox = {}
  /\ wake = [a \in Addrs |-> "none"]
  /\ store = [a \in Addrs |-> NoRec]
  /\ agentAlive = TRUE
  /\ queue = <<>>
  /\ teardown = {}
  /\ nextId = 1
  /\ drops = 0 /\ failures = 0 /\ forgets = 0
  /\ overwrote = FALSE /\ connectedNoKey = FALSE

ArmWakeF(w, a) == IF Radio[a] = "ble" /\ w[a] = "none" THEN [w EXCEPT ![a] = "armed"] ELSE w

ArmWake(a) == wake' = ArmWakeF(wake, a)

SetStage(a, s) == op' = [op EXCEPT ![a].stage = s]

NewOp(kind, stage, reconnect) ==
  [id |-> nextId, kind |-> kind, stage |-> stage, authOk |-> FALSE,
   newKey |-> FALSE, reconnect |-> reconnect]

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

\* finish_connection with the connect_tasks id matching.  A successful
\* result is only inserted while the transport still reports a live link.
FinishConnection(a, ok) ==
  /\ op' = [op EXCEPT ![a] = NoOp]
  /\ connecting' = connecting \ {Radio[a]}
  /\ IF ok
       THEN /\ sessions' = [sessions EXCEPT ![a] = op[a].id]
            /\ overwrote' = (overwrote \/ sessions[a] # 0)
            /\ connectedNoKey' = (connectedNoKey \/ (Radio[a] = "bredr" /\ ~store[a].key))
            /\ UNCHANGED wake
       ELSE /\ IF op[a].reconnect THEN ArmWake(a) ELSE UNCHANGED wake
            /\ UNCHANGED <<sessions, overwrote, connectedNoKey>>

\* Connect handler (user Connect or scan_finished's auto Connect).
Connect(a) ==
  /\ nextId <= MaxIds
  /\ BeginConnect(a, FALSE, wake)
  /\ UNCHANGED <<sessions, phys, mailbox, store, agentAlive, queue, teardown, drops,
                 failures, forgets, overwrote, connectedNoKey>>

\* HciLifecycle(ConnectionRequest): device-initiated BR/EDR reconnect.
HciLifecycle(a) ==
  /\ Radio[a] = "bredr" /\ phys[a] = "down" /\ nextId <= MaxIds
  /\ ~(sessions[a] # 0 \/ op[a] # NoOp \/ "bredr" \in connecting)
  /\ op' = [op EXCEPT ![a] = NewOp("in", "accept", FALSE)]
  /\ connecting' = connecting \cup {"bredr"}
  /\ nextId' = nextId + 1
  /\ UNCHANGED <<sessions, phys, mailbox, wake, store, agentAlive, queue, teardown, drops,
                 failures, forgets, overwrote, connectedNoKey>>

\* accept_inbound: requires a persisted auto-reconnect record, then accepts.
AcceptInbound(a) ==
  /\ op[a].stage = "accept"
  /\ IF store[a].rec
       THEN /\ phys' = [phys EXCEPT ![a] = "up"]
            /\ SetStage(a, "auth")
            /\ queue' = Append(queue, [kind |-> "lkreq", addr |-> a])
            /\ UNCHANGED <<connecting, wake, sessions, overwrote, connectedNoKey>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED <<phys, queue>>
  /\ UNCHANGED <<mailbox, store, agentAlive, teardown, nextId, drops, failures, forgets>>

\* connect(): keys.register, then connect_acl (+ authenticate) or BleGatt::connect.
Acl(a) ==
  /\ op[a].stage = "acl"
  /\ IF phys[a] = "down"
       THEN /\ phys' = [phys EXCEPT ![a] = "up"]
            /\ store' = [store EXCEPT ![a].rec = TRUE]
            /\ IF Radio[a] = "bredr"
                 THEN /\ SetStage(a, "auth")
                      /\ queue' = Append(queue, [kind |-> "lkreq", addr |-> a])
                 ELSE /\ SetStage(a, "hid")
                      /\ UNCHANGED queue
            /\ UNCHANGED <<connecting, wake, sessions, overwrote, connectedNoKey>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED <<phys, store, queue>>
  /\ UNCHANGED <<mailbox, agentAlive, teardown, nextId, drops, failures, forgets>>

\* BrEdrHid::persist: the key captured by authenticate() is written before
\* HID setup.  A failed write abandons the attempt and disconnects.
PersistKey(a) ==
  /\ op[a].stage = "auth" /\ op[a].authOk /\ op[a].newKey
  /\ \/ /\ store' = [store EXCEPT ![a] = [rec |-> TRUE, key |-> TRUE]]
        /\ op' = [op EXCEPT ![a].newKey = FALSE]
        /\ UNCHANGED <<connecting, wake, sessions, phys, failures, overwrote,
                       connectedNoKey>>
     \/ /\ failures < MaxFailures
        /\ failures' = failures + 1
        /\ FinishConnection(a, FALSE)
        /\ phys' = [phys EXCEPT ![a] = "down"]
        /\ UNCHANGED store
  /\ UNCHANGED <<mailbox, agentAlive, queue, teardown, nextId, drops, forgets>>

\* authenticate() returns on a matching DisconnectionComplete; the attempt
\* fails without waiting for the 30 s timeout.
AuthDisconnected(a) ==
  /\ op[a].stage = "auth" /\ phys[a] = "down"
  /\ FinishConnection(a, FALSE)
  /\ UNCHANGED <<phys, mailbox, store, agentAlive, queue, teardown, nextId, drops,
                 failures, forgets>>

\* BrEdrHid channel setup after authenticate() / GATT bond.
OpenHid(a) ==
  /\ \/ op[a].stage = "hid"
     \/ op[a].stage = "auth" /\ op[a].authOk /\ ~op[a].newKey
  /\ IF phys[a] = "up"
       THEN /\ SetStage(a, "sync")
            /\ UNCHANGED <<connecting, wake, sessions, overwrote, connectedNoKey, phys>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED phys
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, teardown, nextId, drops, failures,
                 forgets>>

\* controller_session.synchronize: Ok, or Err -> transport.disconnect.
Synchronize(a) ==
  /\ op[a].stage = "sync"
  /\ SetStage(a, "synced")
  /\ UNCHANGED <<sessions, connecting, phys, mailbox, wake, store, agentAlive, queue,
                 teardown, nextId, drops, failures, forgets, overwrote, connectedNoKey>>

SynchronizeFails(a) ==
  /\ op[a].stage = "sync" /\ phys[a] = "down"
  /\ FinishConnection(a, FALSE)
  /\ UNCHANGED <<phys, mailbox, store, agentAlive, queue, teardown, nextId, drops,
                 failures, forgets>>

\* keys.register, then finish_connection: subscribe, check state(), insert
\* and spawn report forwarding, or drop a transport whose link already ended.
SpawnReports(a) ==
  /\ op[a].stage = "synced"
  /\ store' = [store EXCEPT ![a].rec = TRUE]
  /\ FinishConnection(a, phys[a] = "up")
  /\ UNCHANGED <<phys, mailbox, agentAlive, queue, teardown, nextId, drops, failures,
                 forgets>>

\* time::timeout(CONNECTION_ATTEMPT_TIMEOUT): the future is dropped.  A dropped
\* BrEdrHid spawns host.disconnect for its live handle; a dropped BleGatt
\* releases the peripheral.
Timeout(a) ==
  /\ op[a].stage \in {"accept", "acl", "auth", "hid", "sync"}
  /\ FinishConnection(a, FALSE)
  /\ IF Radio[a] = "ble"
       THEN /\ phys' = [phys EXCEPT ![a] = "down"]
            /\ UNCHANGED teardown
       ELSE /\ teardown' = IF phys[a] = "up" THEN teardown \cup {a} ELSE teardown
            /\ UNCHANGED phys
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, nextId, drops, failures, forgets>>

\* authenticate()'s own 30 s timeout; abandon() disconnects.
AuthTimeout(a) ==
  /\ op[a].stage = "auth" /\ ~op[a].authOk
  /\ FinishConnection(a, FALSE)
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, teardown, nextId, drops, failures,
                 forgets>>

\* The task spawned by BrEdrHid::drop completes host.disconnect.
Teardown(a) ==
  /\ a \in teardown
  /\ teardown' = teardown \ {a}
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ UNCHANGED <<sessions, op, connecting, mailbox, wake, store, agentAlive, queue,
                 nextId, drops, failures, forgets, overwrote, connectedNoKey>>

\* PairingAgent::handle for the head of host.events().  Replies to a link
\* that is already gone fail and are logged.  A pairing exchange produces a
\* LinkKeyNotification that authenticate() captures before
\* AuthenticationComplete.
PairingAgentHandle ==
  /\ agentAlive /\ queue # <<>>
  /\ LET e == Head(queue)
         a == e.addr
         authing == op[a].stage = "auth"
     IN
     CASE e.kind \in {"lkreq", "pin"} /\ phys[a] = "down" ->
            /\ queue' = Tail(queue)
            /\ UNCHANGED <<op, store>>
       [] e.kind = "lkreq" /\ store[a].key ->
            /\ op' = IF authing THEN [op EXCEPT ![a].authOk = TRUE] ELSE op
            /\ queue' = Tail(queue)
            /\ UNCHANGED store
       [] e.kind = "lkreq" ->
            /\ queue' = Append(Tail(queue), [kind |-> "pin", addr |-> a])
            /\ UNCHANGED <<op, store>>
       [] e.kind = "pin" ->
            /\ queue' = Append(Tail(queue), [kind |-> "lknotify", addr |-> a])
            /\ op' = IF authing
                       THEN [op EXCEPT ![a].authOk = TRUE, ![a].newKey = TRUE]
                       ELSE op
            /\ UNCHANGED store
       [] e.kind = "lknotify" ->
            /\ store' = [store EXCEPT ![a] = [rec |-> TRUE, key |-> TRUE]]
            /\ queue' = Tail(queue)
            /\ UNCHANGED op
  /\ UNCHANGED <<sessions, connecting, phys, mailbox, wake, agentAlive, teardown, nextId,
                 drops, failures, forgets, overwrote, connectedNoKey>>

\* The agent's own ControllerLinkKeyStore::put fails; it logs and continues.
LinkKeyPutFails ==
  /\ agentAlive /\ queue # <<>> /\ Head(queue).kind = "lknotify"
  /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ queue' = Tail(queue)
  /\ UNCHANGED <<sessions, op, connecting, phys, mailbox, wake, store, agentAlive,
                 teardown, nextId, drops, forgets, overwrote, connectedNoKey>>

\* Device or radio drops the link.  The transport report task stores
\* DISCONNECTED and broadcasts on `disconnects`; a live session's report
\* forwarding turns that into a SessionEnded message.  Before insertion the
\* retained state() is checked by finish_connection instead.
PhysDrop(a) ==
  /\ phys[a] = "up" /\ drops < MaxDrops
  /\ drops' = drops + 1
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ teardown' = teardown \ {a}
  /\ mailbox' = IF sessions[a] # 0
                  THEN mailbox \cup {[kind |-> "ended", addr |-> a, id |-> sessions[a]]}
                  ELSE mailbox
  /\ UNCHANGED <<sessions, op, connecting, wake, store, agentAlive, queue, nextId,
                 failures, forgets, overwrote, connectedNoKey>>

\* Handler<SessionEnded>.
SessionEnded(m) ==
  /\ m \in mailbox /\ m.kind = "ended"
  /\ mailbox' = mailbox \ {m}
  /\ IF sessions[m.addr] = m.id
       THEN /\ sessions' = [sessions EXCEPT ![m.addr] = 0]
            /\ IF store[m.addr].rec THEN ArmWake(m.addr) ELSE UNCHANGED wake
       ELSE UNCHANGED <<sessions, wake>>
  /\ UNCHANGED <<op, connecting, phys, store, agentAlive, queue, teardown, nextId, drops,
                 failures, forgets, overwrote, connectedNoKey>>

\* arm_ble_wake task finds the advertisement and do_sends WakeDiscovered.
WakeScan(a) ==
  /\ wake[a] = "armed" /\ phys[a] = "down"
  /\ wake' = [wake EXCEPT ![a] = "fired"]
  /\ mailbox' = mailbox \cup {[kind |-> "wake", addr |-> a, id |-> 0]}
  /\ UNCHANGED <<sessions, op, connecting, phys, store, agentAlive, queue, teardown,
                 nextId, drops, failures, forgets, overwrote, connectedNoKey>>

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
  /\ UNCHANGED <<sessions, phys, store, agentAlive, queue, teardown, drops, failures,
                 forgets, overwrote, connectedNoKey>>

\* disconnect_session (Disconnect / Sleep).  Sleep re-arms the BLE wake.
DisconnectSession(a, sleep) ==
  /\ sessions[a] # 0
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ IF sleep /\ store[a].rec THEN ArmWake(a) ELSE UNCHANGED wake
  /\ UNCHANGED <<op, connecting, mailbox, store, agentAlive, queue, teardown, nextId,
                 drops, failures, forgets, overwrote, connectedNoKey>>

\* Handler<Forget>: abort the connect future (a BR/EDR transport's Drop
\* spawns its disconnect), drop the wake task, disconnect the session,
\* pairing.forget + store.remove.
Forget(a) ==
  /\ forgets < MaxForgets
  /\ forgets' = forgets + 1
  /\ op' = [op EXCEPT ![a] = NoOp]
  /\ connecting' = IF op[a] # NoOp THEN connecting \ {Radio[a]} ELSE connecting
  /\ wake' = [wake EXCEPT ![a] = "none"]
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ phys' = IF sessions[a] # 0 \/ (op[a] # NoOp /\ Radio[a] = "ble")
               THEN [phys EXCEPT ![a] = "down"] ELSE phys
  /\ teardown' = IF sessions[a] = 0 /\ op[a] # NoOp /\ Radio[a] = "bredr" /\ phys[a] = "up"
                   THEN teardown \cup {a} ELSE teardown
  /\ store' = [store EXCEPT ![a] = NoRec]
  /\ UNCHANGED <<mailbox, agentAlive, queue, nextId, drops, failures, overwrote,
                 connectedNoKey>>

Quiet ==
  /\ \A a \in Addrs : op[a] = NoOp
  /\ mailbox = {}
  /\ teardown = {}
  /\ (queue = <<>> \/ ~agentAlive)

Finished == Quiet /\ UNCHANGED vars

ConnectProgress(a) ==
  AcceptInbound(a) \/ Acl(a) \/ PersistKey(a) \/ AuthDisconnected(a) \/ OpenHid(a)
  \/ Synchronize(a) \/ SynchronizeFails(a) \/ SpawnReports(a) \/ AuthTimeout(a)

ActorProgress ==
  \/ \E m \in mailbox : SessionEnded(m) \/ WakeDiscovered(m)

Next ==
  \/ \E a \in Addrs :
       \/ Connect(a) \/ HciLifecycle(a) \/ ConnectProgress(a) \/ PhysDrop(a)
       \/ WakeScan(a) \/ DisconnectSession(a, FALSE) \/ DisconnectSession(a, TRUE)
       \/ Timeout(a) \/ Forget(a) \/ Teardown(a)
  \/ PairingAgentHandle \/ LinkKeyPutFails
  \/ ActorProgress
  \/ Finished

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A a \in Addrs : WF_vars(ConnectProgress(a)) /\ WF_vars(Teardown(a))
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

\* The pairing agent survives a device disappearing mid-pairing.
PairingAgentAlive == agentAlive

\* No physical link is left without a session, attempt or pending teardown.
NoOrphanLink ==
  \A a \in Addrs : phys[a] = "up" => sessions[a] # 0 \/ op[a] # NoOp \/ a \in teardown

\* A link handed to teardown is eventually disconnected.
TeardownCompletes == \A a \in Addrs : a \in teardown ~> phys[a] = "down"

\* A session whose link went down is eventually removed.
NoStuckSession ==
  \A a \in Addrs : (sessions[a] # 0 /\ phys[a] = "down") ~> sessions[a] = 0

\* Every connect attempt eventually finishes.
ConnectAttemptEnds == \A a \in Addrs : op[a] # NoOp ~> op[a] = NoOp

=============================================================================
