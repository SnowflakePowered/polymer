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
(*   phys[a]        physical ACL / GATT link of device a                   *)
(*   nextId         next_session_id                                        *)
(*   overwrote      ghost: finish_connection inserted over a live session  *)
(*   connectedNoKey ghost: ManagerEvent::Connected for a BR/EDR device     *)
(*                  whose link key was not yet in the store                *)
(*                                                                         *)
(* Connect future stages (connection/transport.rs, bredr/hid.rs):          *)
(*   accept  accept_inbound: persisted-record check, accept_connection     *)
(*   acl     keys.register + host.connect / BleGatt::connect               *)
(*   auth    HciHost::authenticate waiting for AuthenticationComplete     *)
(*           (it ignores DisconnectionComplete; only the timeout ends it)  *)
(*   hid     L2CAP HID channels (BrEdrHid::finish_connection spawns the    *)
(*           transport report task) / GATT bond                            *)
(*   sync    ControllerSession::synchronize                                *)
(*   synced  synchronize returned Ok; keys.register (file I/O), then       *)
(*           spawn_reports subscribes transport.disconnections() and the  *)
(*           future returns into finish_connection in the same poll        *)
(*                                                                         *)
(* Pairing agent events: lkreq = LinkKeyRequest, pin = PinCodeRequest /    *)
(* IoCapabilityRequest / UserConfirmationRequest (each needs a reply),     *)
(* lknotify = LinkKeyNotification.  AuthenticationComplete is broadcast    *)
(* to authenticate() independently of the agent's queue.  Assumption:      *)
(* a reply command for a link that is already gone completes with a        *)
(* non-zero status, so execute_checked errs and run() returns.            *)
(*                                                                         *)
(* Abstracted: scanning/discovery (devices are always discovered),         *)
(* suspended scans, SDP, player index, external ownership, SendRaw and     *)
(* other atomic session handlers (take + reinsert within one atomic step), *)
(* Shutdown, wake_gate, process crash.  BLE bonding is the OS's.           *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Addrs, Radio, MaxIds, MaxDrops, MaxFailures, MaxForgets

ASSUME Radio \in [Addrs -> {"ble", "bredr"}]

NoOp == [id |-> 0, kind |-> "none", stage |-> "none", authOk |-> FALSE, reconnect |-> FALSE]
NoRec == [rec |-> FALSE, key |-> FALSE]

VARIABLES sessions, op, connecting, phys, mailbox, wake, store,
          agentAlive, queue, nextId, drops, failures, forgets,
          overwrote, connectedNoKey

vars == <<sessions, op, connecting, phys, mailbox, wake, store, agentAlive, queue,
          nextId, drops, failures, forgets, overwrote, connectedNoKey>>

Stages == {"none", "accept", "acl", "auth", "hid", "sync", "synced"}

TypeOK ==
  /\ sessions \in [Addrs -> 0..MaxIds]
  /\ op \in [Addrs -> [id : 0..MaxIds, kind : {"none", "out", "in"}, stage : Stages,
                       authOk : BOOLEAN, reconnect : BOOLEAN]]
  /\ connecting \subseteq {"ble", "bredr"}
  /\ phys \in [Addrs -> {"down", "up"}]
  /\ wake \in [Addrs -> {"none", "armed", "fired"}]
  /\ store \in [Addrs -> [rec : BOOLEAN, key : BOOLEAN]]
  /\ agentAlive \in BOOLEAN

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
  /\ nextId = 1
  /\ drops = 0 /\ failures = 0 /\ forgets = 0
  /\ overwrote = FALSE /\ connectedNoKey = FALSE

ArmWakeF(w, a) == IF Radio[a] = "ble" /\ w[a] = "none" THEN [w EXCEPT ![a] = "armed"] ELSE w

ArmWake(a) == wake' = ArmWakeF(wake, a)

SetStage(a, s) == op' = [op EXCEPT ![a].stage = s]

\* begin_connect: early Ok if a session exists, error if busy (re-arming a
\* reconnect), otherwise start the connect future.
BeginConnect(a, reconnect, w) ==
  IF sessions[a] # 0 THEN /\ wake' = w /\ UNCHANGED <<op, connecting, nextId>>
  ELSE IF op[a] # NoOp \/ Radio[a] \in connecting
    THEN /\ wake' = IF reconnect THEN ArmWakeF(w, a) ELSE w
         /\ UNCHANGED <<op, connecting, nextId>>
  ELSE /\ op' = [op EXCEPT ![a] = [id |-> nextId, kind |-> "out", stage |-> "acl",
                                   authOk |-> FALSE, reconnect |-> reconnect]]
       /\ connecting' = connecting \cup {Radio[a]}
       /\ nextId' = nextId + 1
       /\ wake' = w

\* finish_connection with the connect_tasks id matching.
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
  /\ UNCHANGED <<sessions, phys, mailbox, store, agentAlive, queue, drops, failures,
                 forgets, overwrote, connectedNoKey>>

\* HciLifecycle(ConnectionRequest): device-initiated BR/EDR reconnect.
HciLifecycle(a) ==
  /\ Radio[a] = "bredr" /\ phys[a] = "down" /\ nextId <= MaxIds
  /\ ~(sessions[a] # 0 \/ op[a] # NoOp \/ "bredr" \in connecting)
  /\ op' = [op EXCEPT ![a] = [id |-> nextId, kind |-> "in", stage |-> "accept",
                              authOk |-> FALSE, reconnect |-> FALSE]]
  /\ connecting' = connecting \cup {"bredr"}
  /\ nextId' = nextId + 1
  /\ UNCHANGED <<sessions, phys, mailbox, wake, store, agentAlive, queue, drops, failures,
                 forgets, overwrote, connectedNoKey>>

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
  /\ UNCHANGED <<mailbox, store, agentAlive, nextId, drops, failures, forgets>>

\* connect(): keys.register, then host.connect (+ authenticate) or BleGatt::connect.
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
  /\ UNCHANGED <<mailbox, agentAlive, nextId, drops, failures, forgets>>

\* BrEdrHid channel setup after authenticate() / GATT bond.
OpenHid(a) ==
  /\ \/ op[a].stage = "hid"
     \/ op[a].stage = "auth" /\ op[a].authOk
  /\ IF phys[a] = "up"
       THEN /\ SetStage(a, "sync")
            /\ UNCHANGED <<connecting, wake, sessions, overwrote, connectedNoKey, phys>>
       ELSE /\ FinishConnection(a, FALSE)
            /\ UNCHANGED phys
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, nextId, drops, failures, forgets>>

\* controller_session.synchronize: Ok, or Err -> transport.disconnect.
Synchronize(a) ==
  /\ op[a].stage = "sync"
  /\ SetStage(a, "synced")
  /\ UNCHANGED <<sessions, connecting, phys, mailbox, wake, store, agentAlive, queue,
                 nextId, drops, failures, forgets, overwrote, connectedNoKey>>

SynchronizeFails(a) ==
  /\ op[a].stage = "sync" /\ phys[a] = "down"
  /\ FinishConnection(a, FALSE)
  /\ UNCHANGED <<phys, mailbox, store, agentAlive, queue, nextId, drops, failures, forgets>>

\* keys.register + spawn_reports + return Ok into finish_connection.
SpawnReports(a) ==
  /\ op[a].stage = "synced"
  /\ store' = [store EXCEPT ![a].rec = TRUE]
  /\ FinishConnection(a, TRUE)
  /\ UNCHANGED <<phys, mailbox, agentAlive, queue, nextId, drops, failures, forgets>>

\* time::timeout(CONNECTION_ATTEMPT_TIMEOUT): the future is dropped.  A dropped
\* BrEdrHid closes L2CAP channels but never disconnects the ACL; a dropped
\* BleGatt releases the peripheral.
Timeout(a) ==
  /\ op[a].stage \in {"accept", "acl", "auth", "hid", "sync"}
  /\ FinishConnection(a, FALSE)
  /\ phys' = IF Radio[a] = "ble" THEN [phys EXCEPT ![a] = "down"] ELSE phys
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, nextId, drops, failures, forgets>>

\* authenticate()'s own 30 s timeout; finish_outbound_handle disconnects.
AuthTimeout(a) ==
  /\ op[a].stage = "auth" /\ ~op[a].authOk
  /\ FinishConnection(a, FALSE)
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ UNCHANGED <<mailbox, store, agentAlive, queue, nextId, drops, failures, forgets>>

\* PairingAgent::handle for the head of host.events().
PairingAgentHandle ==
  /\ agentAlive /\ queue # <<>>
  /\ LET e == Head(queue)
         a == e.addr
         authing == op[a].stage = "auth"
     IN
     CASE e.kind \in {"lkreq", "pin"} /\ phys[a] = "down" ->
            /\ agentAlive' = FALSE
            /\ queue' = Tail(queue)
            /\ UNCHANGED <<op, store, failures>>
       [] e.kind = "lkreq" /\ store[a].key ->
            /\ op' = IF authing THEN [op EXCEPT ![a].authOk = TRUE] ELSE op
            /\ queue' = Tail(queue)
            /\ UNCHANGED <<agentAlive, store, failures>>
       [] e.kind = "lkreq" ->
            /\ queue' = Append(Tail(queue), [kind |-> "pin", addr |-> a])
            /\ UNCHANGED <<op, agentAlive, store, failures>>
       [] e.kind = "pin" ->
            /\ queue' = Append(Tail(queue), [kind |-> "lknotify", addr |-> a])
            /\ op' = IF authing THEN [op EXCEPT ![a].authOk = TRUE] ELSE op
            /\ UNCHANGED <<agentAlive, store, failures>>
       [] e.kind = "lknotify" ->
            /\ store' = [store EXCEPT ![a] = [rec |-> TRUE, key |-> TRUE]]
            /\ queue' = Tail(queue)
            /\ UNCHANGED <<op, agentAlive, failures>>
  /\ UNCHANGED <<sessions, connecting, phys, mailbox, wake, nextId, drops, forgets,
                 overwrote, connectedNoKey>>

\* ControllerLinkKeyStore::put fails (store I/O); run() returns the error.
LinkKeyPutFails ==
  /\ agentAlive /\ queue # <<>> /\ Head(queue).kind = "lknotify"
  /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ agentAlive' = FALSE
  /\ queue' = Tail(queue)
  /\ UNCHANGED <<sessions, op, connecting, phys, mailbox, wake, store, nextId, drops,
                 forgets, overwrote, connectedNoKey>>

\* Device or radio drops the link.  The transport report task broadcasts on
\* `disconnects`; only a session whose spawn_reports already subscribed
\* turns that into a SessionEnded message.
PhysDrop(a) ==
  /\ phys[a] = "up" /\ drops < MaxDrops
  /\ drops' = drops + 1
  /\ phys' = [phys EXCEPT ![a] = "down"]
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
  /\ UNCHANGED <<op, connecting, phys, store, agentAlive, queue, nextId, drops, failures,
                 forgets, overwrote, connectedNoKey>>

\* arm_ble_wake task finds the advertisement and do_sends WakeDiscovered.
WakeScan(a) ==
  /\ wake[a] = "armed" /\ phys[a] = "down"
  /\ wake' = [wake EXCEPT ![a] = "fired"]
  /\ mailbox' = mailbox \cup {[kind |-> "wake", addr |-> a, id |-> 0]}
  /\ UNCHANGED <<sessions, op, connecting, phys, store, agentAlive, queue, nextId, drops,
                 failures, forgets, overwrote, connectedNoKey>>

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
  /\ UNCHANGED <<sessions, phys, store, agentAlive, queue, drops, failures, forgets,
                 overwrote, connectedNoKey>>

\* disconnect_session (Disconnect / Sleep).  Sleep re-arms the BLE wake.
DisconnectSession(a, sleep) ==
  /\ sessions[a] # 0
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ phys' = [phys EXCEPT ![a] = "down"]
  /\ IF sleep /\ store[a].rec THEN ArmWake(a) ELSE UNCHANGED wake
  /\ UNCHANGED <<op, connecting, mailbox, store, agentAlive, queue, nextId, drops,
                 failures, forgets, overwrote, connectedNoKey>>

\* Handler<Forget>: abort the connect future, drop the wake task, disconnect
\* the session, pairing.forget + store.remove.
Forget(a) ==
  /\ forgets < MaxForgets
  /\ forgets' = forgets + 1
  /\ op' = [op EXCEPT ![a] = NoOp]
  /\ connecting' = IF op[a] # NoOp THEN connecting \ {Radio[a]} ELSE connecting
  /\ wake' = [wake EXCEPT ![a] = "none"]
  /\ sessions' = [sessions EXCEPT ![a] = 0]
  /\ phys' = IF sessions[a] # 0 \/ (op[a] # NoOp /\ Radio[a] = "ble")
               THEN [phys EXCEPT ![a] = "down"] ELSE phys
  /\ store' = [store EXCEPT ![a] = NoRec]
  /\ UNCHANGED <<mailbox, agentAlive, queue, nextId, drops, failures, overwrote,
                 connectedNoKey>>

Quiet ==
  /\ \A a \in Addrs : op[a] = NoOp
  /\ mailbox = {}
  /\ (queue = <<>> \/ ~agentAlive)

Finished == Quiet /\ UNCHANGED vars

ConnectProgress(a) ==
  AcceptInbound(a) \/ Acl(a) \/ OpenHid(a) \/ Synchronize(a) \/ SynchronizeFails(a)
  \/ SpawnReports(a) \/ AuthTimeout(a)

ActorProgress ==
  \/ \E m \in mailbox : SessionEnded(m) \/ WakeDiscovered(m)

Next ==
  \/ \E a \in Addrs :
       \/ Connect(a) \/ HciLifecycle(a) \/ ConnectProgress(a) \/ PhysDrop(a)
       \/ WakeScan(a) \/ DisconnectSession(a, FALSE) \/ DisconnectSession(a, TRUE)
       \/ Timeout(a) \/ Forget(a)
  \/ PairingAgentHandle \/ LinkKeyPutFails
  \/ ActorProgress
  \/ Finished

Spec ==
  /\ Init /\ [][Next]_vars
  /\ \A a \in Addrs : WF_vars(ConnectProgress(a))
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

\* No physical link is left without a session or attempt owning it.
NoOrphanLink == \A a \in Addrs : phys[a] = "up" => sessions[a] # 0 \/ op[a] # NoOp

\* A session whose link went down is eventually removed.
NoStuckSession ==
  \A a \in Addrs : (sessions[a] # 0 /\ phys[a] = "down") ~> sessions[a] = 0

\* Every connect attempt eventually finishes.
ConnectAttemptEnds == \A a \in Addrs : op[a] # NoOp ~> op[a] = NoOp

=============================================================================
