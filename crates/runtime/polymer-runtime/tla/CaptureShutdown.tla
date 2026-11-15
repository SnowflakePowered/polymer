--------------------------- MODULE CaptureShutdown ---------------------------
(***************************************************************************)
(* Capture transport shutdown racing an accepted surface offer.            *)
(*                                                                         *)
(* Rust mapping (src/capture.rs, src/instance/manager.rs):                 *)
(*   offer     the oneshot a receive thread waits on in receive_connection *)
(*             after (callbacks.offer): "pending", "accepted" (sent true   *)
(*             by SurfaceManager::resolve_offer), "rejected"               *)
(*   ui        the thread calling SurfaceManager::shutdown:                *)
(*             "idle", "manager" = the `topology`-locked section that      *)
(*             rejects pending offers and revokes active connections,      *)
(*             "stop" = CaptureWorkers::shutdown setting `stopping`,       *)
(*             shutting sockets and closing timelines, "lock" = taking     *)
(*             `connections`, "join" = joining the drained handles,        *)
(*             "done"                                                      *)
(*   recv      receive_connection for one connection:                      *)
(*             "await" = response.recv(), "insert" = timelines.insert,     *)
(*             "lock"/"push" = connections.lock().push(watcher),           *)
(*             "send" = send(StreamAccepted), "receive" = the next         *)
(*             socket.receive(), "revoke" = CaptureShared::revoke and the  *)
(*             disconnected callback, "done"                               *)
(*   watcher   the polymer-runtime-frames thread running watch_frames      *)
(*   timeline  the connection's ProducerTimeline: "none", "open", "closed" *)
(*   conns     CaptureWorkers::connections, in push order                  *)
(*   joining   the handles drained from conns that ui still has to join    *)
(*   lock      holder of the `connections` Mutex                           *)
(*   shut      the connection's socket clone in `sockets` was shut down;   *)
(*             the clone shares the stream (Windows shutdown(Both), macOS  *)
(*             connection.close()), so send and receive on it fail         *)
(*                                                                         *)
(* The acceptor thread is joined before `connections` is taken and has     *)
(* already pushed the receive thread's handle, so it is not modelled.      *)
(*                                                                         *)
(* Fixed = TRUE models the proposed change: shutdown takes the vector out  *)
(* of the Mutex and joins without holding it, and the receive thread       *)
(* checks `stopping` under the Mutex before spawning a watcher.            *)
(***************************************************************************)
EXTENDS Integers, Sequences, TLC

CONSTANTS Fixed

None == "none"

VARIABLES offer, ui, recv, watcher, timeline, conns, joining, lock, stopping, shut

vars == <<offer, ui, recv, watcher, timeline, conns, joining, lock, stopping, shut>>

TypeOK ==
  /\ offer \in {"pending", "accepted", "rejected"}
  /\ ui \in {"idle", "manager", "stop", "lock", "join", "done"}
  /\ recv \in {"await", "insert", "lock", "push", "send", "receive", "revoke", "done"}
  /\ watcher \in {"none", "running", "done"}
  /\ timeline \in {"none", "open", "closed"}
  /\ conns \in Seq({"recv", "watcher"})
  /\ joining \in Seq({"recv", "watcher"})
  /\ lock \in {None, "ui", "recv"}
  /\ stopping \in BOOLEAN
  /\ shut \in BOOLEAN

Init ==
  /\ offer = "pending"
  /\ ui = "idle"
  /\ recv = "await"
  /\ watcher = "none"
  /\ timeline = "none"
  /\ conns = <<"recv">>
  /\ joining = <<>>
  /\ lock = None
  /\ stopping = FALSE
  /\ shut = FALSE

Exited(h) == IF h = "recv" THEN recv = "done" ELSE watcher = "done"

CloseTimeline == timeline' = IF timeline = "open" THEN "closed" ELSE timeline

Resolve ==
  /\ ui = "idle"
  /\ offer = "pending"
  /\ offer' \in {"accepted", "rejected"}
  /\ shut' = (offer' = "rejected")
  /\ UNCHANGED <<ui, recv, watcher, timeline, conns, joining, lock, stopping>>

BeginShutdown ==
  /\ ui = "idle"
  /\ ui' = "manager"
  /\ UNCHANGED <<offer, recv, watcher, timeline, conns, joining, lock, stopping, shut>>

Manager ==
  /\ ui = "manager"
  /\ offer' = IF offer = "pending" THEN "rejected" ELSE offer
  /\ shut' = TRUE
  /\ CloseTimeline
  /\ ui' = "stop"
  /\ UNCHANGED <<recv, watcher, conns, joining, lock, stopping>>

Stop ==
  /\ ui = "stop"
  /\ stopping' = TRUE
  /\ shut' = TRUE
  /\ CloseTimeline
  /\ ui' = "lock"
  /\ UNCHANGED <<offer, recv, watcher, conns, joining, lock>>

Drain ==
  /\ ui = "lock"
  /\ lock = None
  /\ joining' = conns
  /\ conns' = <<>>
  /\ lock' = IF Fixed THEN None ELSE "ui"
  /\ ui' = "join"
  /\ UNCHANGED <<offer, recv, watcher, timeline, stopping, shut>>

Join ==
  /\ ui = "join"
  /\ joining # <<>>
  /\ Exited(Head(joining))
  /\ joining' = Tail(joining)
  /\ UNCHANGED <<offer, ui, recv, watcher, timeline, conns, lock, stopping, shut>>

FinishShutdown ==
  /\ ui = "join"
  /\ joining = <<>>
  /\ lock' = IF lock = "ui" THEN None ELSE lock
  /\ ui' = "done"
  /\ UNCHANGED <<offer, recv, watcher, timeline, conns, joining, stopping, shut>>

UiStep == Resolve \/ BeginShutdown \/ Manager \/ Stop \/ Drain \/ Join \/ FinishShutdown

RecvWake ==
  /\ recv = "await"
  /\ offer # "pending"
  /\ recv' = IF offer = "accepted" THEN "insert" ELSE "revoke"
  /\ UNCHANGED <<offer, ui, watcher, timeline, conns, joining, lock, stopping, shut>>

RecvInsert ==
  /\ recv = "insert"
  /\ timeline' = "open"
  /\ recv' = "lock"
  /\ UNCHANGED <<offer, ui, watcher, conns, joining, lock, stopping, shut>>

RecvLock ==
  /\ recv = "lock"
  /\ lock = None
  /\ lock' = "recv"
  /\ recv' = "push"
  /\ UNCHANGED <<offer, ui, watcher, timeline, conns, joining, stopping, shut>>

RecvPush ==
  /\ recv = "push"
  /\ lock' = None
  /\ IF Fixed /\ stopping
       THEN /\ recv' = "revoke"
            /\ UNCHANGED <<watcher, conns>>
       ELSE /\ conns' = Append(conns, "watcher")
            /\ watcher' = "running"
            /\ recv' = "send"
  /\ UNCHANGED <<offer, ui, timeline, joining, stopping, shut>>

RecvSend ==
  /\ recv = "send"
  /\ recv' = IF shut THEN "revoke" ELSE "receive"
  /\ UNCHANGED <<offer, ui, watcher, timeline, conns, joining, lock, stopping, shut>>

RecvReceive ==
  /\ recv = "receive"
  /\ shut
  /\ recv' = "revoke"
  /\ UNCHANGED <<offer, ui, watcher, timeline, conns, joining, lock, stopping, shut>>

RecvRevoke ==
  /\ recv = "revoke"
  /\ CloseTimeline
  /\ recv' = "done"
  /\ UNCHANGED <<offer, ui, watcher, conns, joining, lock, stopping, shut>>

RecvStep == RecvWake \/ RecvInsert \/ RecvLock \/ RecvPush \/ RecvSend \/ RecvReceive \/ RecvRevoke

WatcherExit ==
  /\ watcher = "running"
  /\ timeline = "closed"
  /\ watcher' = "done"
  /\ UNCHANGED <<offer, ui, recv, timeline, conns, joining, lock, stopping, shut>>

Done ==
  /\ ui = "done"
  /\ recv = "done"
  /\ watcher # "running"
  /\ UNCHANGED vars

Next == UiStep \/ RecvStep \/ WatcherExit \/ Done

Spec == Init /\ [][Next]_vars
        /\ WF_vars(BeginShutdown \/ Manager \/ Stop \/ Drain \/ Join \/ FinishShutdown)
        /\ WF_vars(RecvStep)
        /\ WF_vars(WatcherExit)

NoJoinCycle ==
  ~(/\ ui = "join"
    /\ lock = "ui"
    /\ joining # <<>>
    /\ Head(joining) = "recv"
    /\ recv = "lock")

ShutdownCompletes == <>(ui = "done")

ThreadsExit == <>[](recv = "done" /\ watcher # "running")
=============================================================================
