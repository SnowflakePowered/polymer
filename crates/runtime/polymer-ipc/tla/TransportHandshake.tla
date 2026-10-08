-------------------------- MODULE TransportHandshake --------------------------
(***************************************************************************)
(* T7a: capture stream handshake end to end (Windows path).               *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   pp[c]       producer of connection c, polymer-presentation            *)
(*               producer/session.rs ProducerSession::connect + publish,   *)
(*               producer/export.rs Export::prepare/duplicate/transfer/    *)
(*               drop. "hello" = waiting for Hello, "prepare" =            *)
(*               Export::prepare duplicating, "transfer" = all duplicated, *)
(*               "send" = socket.send(StreamReady), "await" =              *)
(*               socket.receive for StreamAccepted, "accepted", "exited"   *)
(*               (ProducerSession::drop after use), "failed" (publish/     *)
(*               connect returned Err, session dropped), "crashed"         *)
(*   h[c][k]     each DuplicateHandle'd handle of c's StreamReady:         *)
(*               k = "img" (image_handles), "ready"                        *)
(*               (timeline_semaphore_handle), "rel"                        *)
(*               (consumer_semaphore_handle). "exported" = in viewer,      *)
(*               owned by Export (published = false); "viewer" = in        *)
(*               viewer after Export::transfer; "closedV" = CloseHandle    *)
(*               in viewer; "reclaimed" = Export::drop                     *)
(*               DUPLICATE_CLOSE_SOURCE; "gone" = viewer process exited    *)
(*   doubleClose ghost: a close hit a handle the closer did not own        *)
(*   sock[c]     the LocalSocket pair (control/windows.rs); "shut" after   *)
(*               shutdown, drop or process exit on either side             *)
(*   hello[c], ready[c], acc[c]   Message::Hello / StreamReady /           *)
(*               StreamAccepted written to the socket                      *)
(*   wp[c]       polymer-runtime capture.rs receive_connection worker:     *)
(*               "recv" = socket.receive, "timeline" =                     *)
(*               ProducerTimeline::open (polymer-consumer-d3d12            *)
(*               timeline.rs, open_fence closes the ready handle),         *)
(*               "offer" = callbacks.offer, "wait" = response.recv,        *)
(*               "send" = socket.send(StreamAccepted), "exit" = loop       *)
(*               returned (shared.revoke + callbacks.disconnected)         *)
(*   revoked, timeline   CaptureShared::revoked / timelines entries        *)
(*   pending, active, replacing   SurfaceManager::pending / active /       *)
(*               PendingOffer::replacing for the single tracked surface    *)
(*               all connections offer (instance/manager.rs)               *)
(*   resp[c]     the oneshot `accepted` of c's offer                       *)
(*   respTwice   ghost: a oneshot was sent twice                           *)
(*   events      SurfaceEvent::Offered queue to the GUI                    *)
(*   gpc, gcur   polymer-gui game_host/surfaces.rs handle_surface_offer /  *)
(*               accept_surface_offer: import_surface                      *)
(*               (D3d12ConsumerDevice::import closes img and rel), then    *)
(*               capture.accept -> SurfaceOffer::accept -> resolve_offer   *)
(*   imported[c] ghost: c's handles were imported by the GUI               *)
(*   viewerAlive viewer (polymer-gui + polymer-runtime) process alive      *)
(*                                                                         *)
(* All connections offer the same protocol surface_id; Rank is the hook's  *)
(* position in config.hooks.graphics (lower is preferred).                 *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Conns, Rank, GuiMayReject, MaxCrashes, MaxViewerCrashes, MaxFailures

ASSUME Rank \in [Conns -> Nat]

None == "none"
Kinds == {"img", "ready", "rel"}
DupOrder == <<"img", "ready", "rel">>
ProducerDone == {"exited", "failed", "crashed"}
Live == {"exported", "viewer"}

VARIABLES pp, h, doubleClose, sock, hello, ready, acc,
          wp, revoked, timeline, pending, active, replacing, resp, respTwice,
          events, gpc, gcur, imported, viewerAlive, crashes, vcrashes, failures

vars == <<pp, h, doubleClose, sock, hello, ready, acc,
          wp, revoked, timeline, pending, active, replacing, resp, respTwice,
          events, gpc, gcur, imported, viewerAlive, crashes, vcrashes, failures>>

producerVars == <<pp, h, doubleClose, sock, hello, ready, acc>>
viewerVars == <<wp, revoked, timeline, pending, active, replacing, resp, respTwice, events, gpc, gcur, imported>>
budgetVars == <<viewerAlive, crashes, vcrashes, failures>>

TypeOK ==
  /\ pp \in [Conns -> {"connecting", "hello", "prepare", "transfer", "send", "await",
                       "accepted"} \cup ProducerDone]
  /\ h \in [Conns -> [Kinds -> {"none", "exported", "viewer", "closedV", "reclaimed", "gone"}]]
  /\ sock \in [Conns -> {"open", "shut"}]
  /\ ready \in [Conns -> {"none", "sent", "recv"}]
  /\ wp \in [Conns -> {"idle", "recv", "timeline", "offer", "wait", "send", "exit", "done", "dead"}]
  /\ pending \in Conns \cup {None} /\ active \in Conns \cup {None}
  /\ replacing \in [Conns -> Conns \cup {None}]
  /\ resp \in [Conns -> {"none", "true", "false"}]
  /\ events \in Seq(Conns)
  /\ gpc \in {"idle", "imported", "dead"} /\ gcur \in Conns \cup {None}

Init ==
  /\ pp = [c \in Conns |-> "connecting"]
  /\ h = [c \in Conns |-> [k \in Kinds |-> "none"]]
  /\ doubleClose = FALSE
  /\ sock = [c \in Conns |-> "open"]
  /\ hello = [c \in Conns |-> FALSE]
  /\ ready = [c \in Conns |-> "none"]
  /\ acc = [c \in Conns |-> FALSE]
  /\ wp = [c \in Conns |-> "idle"]
  /\ revoked = [c \in Conns |-> FALSE]
  /\ timeline = [c \in Conns |-> FALSE]
  /\ pending = None /\ active = None
  /\ replacing = [c \in Conns |-> None]
  /\ resp = [c \in Conns |-> "none"]
  /\ respTwice = FALSE
  /\ events = <<>>
  /\ gpc = "idle" /\ gcur = None
  /\ imported = [c \in Conns |-> FALSE]
  /\ viewerAlive = TRUE
  /\ crashes = 0 /\ vcrashes = 0 /\ failures = 0

\* CloseHandle in the viewer of the kinds in ks for connection c.
CloseInViewer(hh, c, ks) == [hh EXCEPT ![c] = [k \in Kinds |-> IF k \in ks THEN "closedV" ELSE hh[c][k]]]
BadClose(c, ks) == \E k \in ks : h[c][k] # "viewer"

\* oneshot TxOneshot::send, applied to a resp function.
SendResp(r, c, v) == [r EXCEPT ![c] = v]
TwiceIf(c) == resp[c] # "none"

--------------------------------------------------------------------------
\* Producer: ProducerSession and Export (polymer-presentation).

\* spawn_acceptor: listener.accept, send Hello, sockets.insert, spawn worker.
Accept(c) ==
  /\ viewerAlive /\ pp[c] = "connecting"
  /\ pp' = [pp EXCEPT ![c] = "hello"]
  /\ hello' = [hello EXCEPT ![c] = TRUE]
  /\ wp' = [wp EXCEPT ![c] = "recv"]
  /\ UNCHANGED <<h, doubleClose, sock, ready, acc, revoked, timeline, pending, active, replacing,
                 resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* ProducerSession::connect: socket.receive() -> Hello.
ReceiveHello(c) ==
  /\ pp[c] = "hello" /\ hello[c]
  /\ pp' = [pp EXCEPT ![c] = "prepare"]
  /\ UNCHANGED <<h, doubleClose, sock, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

NextDup(c) == CHOOSE i \in 1..3 : h[c][DupOrder[i]] = "none"

\* Export::duplicate for the next handle of Export::prepare.
Duplicate(c) ==
  /\ pp[c] = "prepare" /\ viewerAlive
  /\ LET k == DupOrder[NextDup(c)] IN
       /\ h' = [h EXCEPT ![c][k] = "exported"]
       /\ pp' = [pp EXCEPT ![c] = IF k = "rel" THEN "transfer" ELSE "prepare"]
  /\ UNCHANGED <<doubleClose, sock, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* DuplicateHandle fails (or the viewer is gone): `?` drops Export with
\* published = false, reclaiming every duplicate; publish returns Err and the
\* session is dropped.
DuplicateFails(c) ==
  /\ pp[c] = "prepare"
  /\ IF viewerAlive THEN failures < MaxFailures /\ failures' = failures + 1
                    ELSE failures' = failures
  /\ h' = [h EXCEPT ![c] = [k \in Kinds |-> IF h[c][k] = "exported" THEN "reclaimed" ELSE h[c][k]]]
  /\ doubleClose' = doubleClose
  /\ pp' = [pp EXCEPT ![c] = "failed"]
  /\ sock' = [sock EXCEPT ![c] = "shut"]
  /\ UNCHANGED <<hello, ready, acc>> /\ UNCHANGED viewerVars
  /\ UNCHANGED <<viewerAlive, crashes, vcrashes>>

\* Export::transfer: published = true before the StreamReady write.
Transfer(c) ==
  /\ pp[c] = "transfer"
  /\ h' = [h EXCEPT ![c] = [k \in Kinds |-> IF h[c][k] = "exported" THEN "viewer" ELSE h[c][k]]]
  /\ pp' = [pp EXCEPT ![c] = "send"]
  /\ UNCHANGED <<doubleClose, sock, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* socket.send(Message::StreamReady(ready)).
SendStreamReady(c) ==
  /\ pp[c] = "send"
  /\ IF sock[c] = "open"
       THEN /\ ready' = [ready EXCEPT ![c] = "sent"]
            /\ pp' = [pp EXCEPT ![c] = "await"]
            /\ UNCHANGED sock
       ELSE /\ pp' = [pp EXCEPT ![c] = "failed"]
            /\ UNCHANGED <<ready, sock>>
  /\ UNCHANGED <<h, doubleClose, hello, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* socket.receive() in publish: StreamAccepted, or EOF -> Rejected.
ReceiveResponse(c) ==
  /\ pp[c] = "await"
  /\ acc[c] \/ sock[c] = "shut"
  /\ pp' = [pp EXCEPT ![c] = IF acc[c] THEN "accepted" ELSE "failed"]
  /\ sock' = IF acc[c] THEN sock ELSE [sock EXCEPT ![c] = "shut"]
  /\ UNCHANGED <<h, doubleClose, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* set_read_timeout(2 s) expires on Hello or StreamAccepted; session dropped.
ReadTimeout(c) ==
  /\ pp[c] \in {"hello", "await"}
  /\ ~(pp[c] = "await" /\ acc[c])
  /\ pp' = [pp EXCEPT ![c] = "failed"]
  /\ sock' = [sock EXCEPT ![c] = "shut"]
  /\ UNCHANGED <<h, doubleClose, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* The swapchain retires: ProducerSession::drop shuts the socket down.
ProducerExit(c) ==
  /\ pp[c] = "accepted"
  /\ pp' = [pp EXCEPT ![c] = "exited"]
  /\ sock' = [sock EXCEPT ![c] = "shut"]
  /\ UNCHANGED <<h, doubleClose, hello, ready, acc>> /\ UNCHANGED viewerVars /\ UNCHANGED budgetVars

\* Producer process dies: no Drop runs; handles already in the viewer stay.
ProducerCrash(c) ==
  /\ pp[c] \notin ProducerDone \cup {"connecting"} /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ pp' = [pp EXCEPT ![c] = "crashed"]
  /\ sock' = [sock EXCEPT ![c] = "shut"]
  /\ UNCHANGED <<h, doubleClose, hello, ready, acc>> /\ UNCHANGED viewerVars
  /\ UNCHANGED <<viewerAlive, vcrashes, failures>>

--------------------------------------------------------------------------
\* Viewer: receive_connection worker (polymer-runtime capture.rs).

\* socket.receive(); then the revoked/stopping check.
WorkerReceive(c) ==
  /\ wp[c] = "recv"
  /\ \/ /\ ready[c] = "sent"
        /\ ready' = [ready EXCEPT ![c] = "recv"]
        /\ wp' = [wp EXCEPT ![c] = IF revoked[c] THEN "exit" ELSE "timeline"]
     \/ /\ ready[c] # "sent" /\ sock[c] = "shut"
        /\ wp' = [wp EXCEPT ![c] = "exit"]
        /\ UNCHANGED ready
  /\ UNCHANGED <<pp, h, doubleClose, sock, hello, acc, revoked, timeline, pending, active, replacing,
                 resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* ProducerTimeline::open succeeds: open_fence closes the ready handle.
TimelineOpen(c) ==
  /\ wp[c] = "timeline"
  /\ h' = CloseInViewer(h, c, {"ready"})
  /\ doubleClose' = (doubleClose \/ BadClose(c, {"ready"}))
  /\ wp' = [wp EXCEPT ![c] = "offer"]
  /\ UNCHANGED <<pp, sock, hello, ready, acc, revoked, timeline, pending, active, replacing,
                 resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* ProducerTimeline::open fails: before open_fence (factory/adapter/device)
\* nothing is closed; inside open_fence the handle is closed first.
TimelineOpenFails(c) ==
  /\ wp[c] = "timeline" /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ \E ks \in {{}, {"ready"}} :
       /\ h' = CloseInViewer(h, c, ks)
       /\ doubleClose' = (doubleClose \/ BadClose(c, ks))
  /\ wp' = [wp EXCEPT ![c] = "exit"]
  /\ UNCHANGED <<pp, sock, hello, ready, acc, revoked, timeline, pending, active, replacing,
                 resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED <<viewerAlive, crashes, vcrashes>>

\* CaptureRouter::revoke applied to x (shutdown of the cloned socket).
RevokeOf(x, rv, tl, sk) ==
  <<[rv EXCEPT ![x] = TRUE], [tl EXCEPT ![x] = FALSE], [sk EXCEPT ![x] = "shut"]>>

\* SurfaceManager::offer for the single tracked surface.
Offer(c) ==
  /\ wp[c] = "offer"
  /\ wp' = [wp EXCEPT ![c] = "wait"]
  /\ IF (pending # None /\ Rank[c] > Rank[pending])
        \/ (pending = None /\ active # None /\ Rank[c] > Rank[active])
       THEN /\ resp' = SendResp(resp, c, "false")
            /\ respTwice' = (respTwice \/ TwiceIf(c))
            /\ UNCHANGED <<pending, replacing, revoked, timeline, sock, events>>
     ELSE IF pending # None
       THEN LET r == RevokeOf(pending, revoked, timeline, sock) IN
            /\ resp' = SendResp(resp, pending, "false")
            /\ respTwice' = (respTwice \/ TwiceIf(pending))
            /\ revoked' = [r[1] EXCEPT ![c] = FALSE]
            /\ timeline' = r[2] /\ sock' = r[3]
            /\ replacing' = [replacing EXCEPT ![c] = active]
            /\ pending' = c
            /\ events' = Append(events, c)
     ELSE /\ revoked' = [revoked EXCEPT ![c] = FALSE]
          /\ replacing' = [replacing EXCEPT ![c] = active]
          /\ pending' = c
          /\ events' = Append(events, c)
          /\ UNCHANGED <<resp, respTwice, timeline, sock>>
  /\ UNCHANGED <<pp, h, doubleClose, hello, ready, acc, active, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* response.recv(); on true: timelines.insert, spawn watch_frames.
WorkerResolved(c) ==
  /\ wp[c] = "wait" /\ resp[c] # "none"
  /\ wp' = [wp EXCEPT ![c] = IF resp[c] = "true" THEN "send" ELSE "exit"]
  /\ timeline' = IF resp[c] = "true" THEN [timeline EXCEPT ![c] = TRUE] ELSE timeline
  /\ UNCHANGED <<pp, h, doubleClose, sock, hello, ready, acc, revoked, pending, active, replacing,
                 resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* socket.send(Message::StreamAccepted)?
SendStreamAccepted(c) ==
  /\ wp[c] = "send"
  /\ IF sock[c] = "open"
       THEN acc' = [acc EXCEPT ![c] = TRUE] /\ wp' = [wp EXCEPT ![c] = "recv"]
       ELSE UNCHANGED acc /\ wp' = [wp EXCEPT ![c] = "exit"]
  /\ UNCHANGED <<pp, h, doubleClose, sock, hello, ready, revoked, timeline, pending, active,
                 replacing, resp, respTwice, events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* receive_connection returns: CaptureShared::revoke, socket dropped,
\* SurfaceManager::disconnected.
WorkerExit(c) ==
  /\ wp[c] = "exit"
  /\ wp' = [wp EXCEPT ![c] = "done"]
  /\ revoked' = [revoked EXCEPT ![c] = TRUE]
  /\ timeline' = [timeline EXCEPT ![c] = FALSE]
  /\ sock' = [sock EXCEPT ![c] = "shut"]
  /\ active' = IF active = c THEN None ELSE active
  /\ UNCHANGED <<pp, h, doubleClose, hello, ready, acc, pending, replacing, resp, respTwice,
                 events, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

--------------------------------------------------------------------------
\* GUI: SurfaceOffer handling and SurfaceManager::resolve_offer.

\* resolve_offer(offer, false) for c (and the Unavailable no-op).
ResolveReject(c) ==
  IF pending = c
    THEN LET r == RevokeOf(c, revoked, timeline, sock) IN
         /\ pending' = None
         /\ resp' = SendResp(resp, c, "false")
         /\ respTwice' = (respTwice \/ TwiceIf(c))
         /\ revoked' = r[1] /\ timeline' = r[2] /\ sock' = r[3]
    ELSE UNCHANGED <<pending, resp, respTwice, revoked, timeline, sock>>

\* capture.reject(offer) / SurfaceOffer::drop / reject_pending_offer.
GuiReject ==
  /\ GuiMayReject /\ gpc = "idle" /\ events # <<>>
  /\ events' = Tail(events)
  /\ ResolveReject(Head(events))
  /\ UNCHANGED <<pp, h, doubleClose, hello, ready, acc, wp, active, replacing, gpc, gcur, imported>>
  /\ UNCHANGED budgetVars

\* import_surface -> D3d12ConsumerDevice::import succeeds: image handles and
\* the consumer fence handle are closed after OpenSharedHandle.
GuiImport ==
  /\ gpc = "idle" /\ events # <<>>
  /\ LET c == Head(events) IN
       /\ h' = CloseInViewer(h, c, {"img", "rel"})
       /\ doubleClose' = (doubleClose \/ BadClose(c, {"img", "rel"}))
       /\ imported' = [imported EXCEPT ![c] = TRUE]
       /\ gpc' = "imported" /\ gcur' = c
  /\ events' = Tail(events)
  /\ UNCHANGED <<pp, sock, hello, ready, acc, wp, revoked, timeline, pending, active, replacing,
                 resp, respTwice>>
  /\ UNCHANGED budgetVars

\* import fails: validate_stream closes nothing, a failed OpenSharedHandle
\* closes the images opened so far, a failed open_fence closes both; the
\* offer is then dropped (resolve_offer false).
GuiImportFails ==
  /\ gpc = "idle" /\ events # <<>> /\ failures < MaxFailures
  /\ failures' = failures + 1
  /\ LET c == Head(events) IN
       /\ \E ks \in {{}, {"img"}, {"img", "rel"}} :
            /\ h' = CloseInViewer(h, c, ks)
            /\ doubleClose' = (doubleClose \/ BadClose(c, ks))
       /\ ResolveReject(c)
  /\ events' = Tail(events)
  /\ UNCHANGED <<pp, hello, ready, acc, wp, active, replacing, gpc, gcur, imported>>
  /\ UNCHANGED <<viewerAlive, crashes, vcrashes>>

\* capture.accept -> SurfaceOffer::accept -> resolve_offer(offer, true);
\* Unavailable -> compositor.remove_surface.
GuiAccept ==
  /\ gpc = "imported"
  /\ LET c == gcur IN
       IF pending = c
         THEN LET r == IF replacing[c] # None
                         THEN RevokeOf(replacing[c], revoked, timeline, sock)
                         ELSE <<revoked, timeline, sock>> IN
              /\ pending' = None
              /\ active' = c
              /\ resp' = SendResp(resp, c, "true")
              /\ respTwice' = (respTwice \/ TwiceIf(c))
              /\ revoked' = r[1] /\ timeline' = r[2] /\ sock' = r[3]
         ELSE UNCHANGED <<pending, active, resp, respTwice, revoked, timeline, sock>>
  /\ gpc' = "idle" /\ gcur' = None
  /\ UNCHANGED <<pp, h, doubleClose, hello, ready, acc, wp, replacing, events, imported>>
  /\ UNCHANGED budgetVars

\* The viewer process exits: its handle table and threads vanish.
ViewerCrash ==
  /\ viewerAlive /\ vcrashes < MaxViewerCrashes
  /\ vcrashes' = vcrashes + 1
  /\ viewerAlive' = FALSE
  /\ h' = [c \in Conns |-> [k \in Kinds |-> IF h[c][k] \in Live THEN "gone" ELSE h[c][k]]]
  /\ sock' = [c \in Conns |-> "shut"]
  /\ wp' = [c \in Conns |-> IF wp[c] = "idle" THEN "idle" ELSE "dead"]
  /\ gpc' = "dead" /\ gcur' = None /\ events' = <<>>
  /\ pending' = None /\ active' = None
  /\ UNCHANGED <<pp, doubleClose, hello, ready, acc, revoked, timeline, replacing, resp, respTwice,
                 imported, crashes, failures>>

Quiescent ==
  /\ \A c \in Conns : pp[c] \in ProducerDone \cup {"connecting"}
  /\ \A c \in Conns : wp[c] \in {"idle", "done", "dead"}
  /\ events = <<>> /\ gpc # "imported"

Finished == Quiescent /\ UNCHANGED vars

ProducerStep(c) ==
  \/ Accept(c) \/ ReceiveHello(c) \/ Duplicate(c) \/ Transfer(c) \/ SendStreamReady(c)
  \/ ReceiveResponse(c) \/ ProducerExit(c)
WorkerStep(c) ==
  \/ WorkerReceive(c) \/ TimelineOpen(c) \/ Offer(c) \/ WorkerResolved(c)
  \/ SendStreamAccepted(c) \/ WorkerExit(c)

Next ==
  \/ \E c \in Conns : ProducerStep(c) \/ DuplicateFails(c) \/ ReadTimeout(c) \/ ProducerCrash(c)
                      \/ WorkerStep(c) \/ TimelineOpenFails(c)
  \/ GuiReject \/ GuiImport \/ GuiImportFails \/ GuiAccept \/ ViewerCrash
  \/ Finished

\* A producer stuck waiting on a vanished peer is released by its read timeout.
Spec == Init /\ [][Next]_vars
        /\ (\A c \in Conns : WF_vars(ProducerStep(c)) /\ WF_vars(WorkerStep(c))
                             /\ WF_vars(pp[c] \in {"hello", "await"} /\ ~viewerAlive /\ ReadTimeout(c))
                             /\ WF_vars(pp[c] = "prepare" /\ ~viewerAlive /\ DuplicateFails(c)))
        /\ WF_vars(GuiReject \/ GuiImport) /\ WF_vars(GuiAccept)

NoDoubleClose == ~doubleClose
OneshotSentOnce == ~respTwice
ActiveWasAccepted == active # None => resp[active] = "true" /\ imported[active]
AtMostOneAcceptedLive ==
  \A c, d \in Conns : c # d /\ pp[c] = "accepted" /\ pp[d] = "accepted"
                      => sock[c] = "shut" \/ sock[d] = "shut"

\* At quiescence no duplicated handle is still open in a live viewer.
NoLeakedHandle ==
  Quiescent /\ viewerAlive => \A c \in Conns, k \in Kinds : h[c][k] \notin Live

ProducerTerminates ==
  \A c \in Conns : <>(pp[c] \in ProducerDone \cup {"accepted"} \/ (pp[c] = "connecting" /\ ~viewerAlive))
ConnectionCleared ==
  \A c \in Conns : pp[c] \in ProducerDone ~> (active # c /\ wp[c] \in {"done", "dead"})
=============================================================================
