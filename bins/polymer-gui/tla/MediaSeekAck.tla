--------------------------- MODULE MediaSeekAck ---------------------------
(***************************************************************************)
(* Shell <video> seek acknowledgements racing element detach.              *)
(*                                                                         *)
(* Rust mapping (src/shell/document/media/video.rs, media/mod.rs,          *)
(*               src/shell/document/mutations.rs,                          *)
(*               polymer-composition-windows/src/surfaces/video/mod.rs):   *)
(*   gen       MediaStatus::generation; NodeSignal drops signals whose     *)
(*             generation differs (MediaPlayers::signal)                   *)
(*   feed      VideoPlayback::feed.is_some(), i.e. a video session is open *)
(*   seeks     VideoPlayback::seeks; Position events are swallowed while   *)
(*             it is non-zero                                              *)
(*   nonzero   VideoPlayback::position is non-zero, so open() sends an     *)
(*             uncounted restore Seek                                      *)
(*   controls  Seek controls queued to the VideoSession of generation gen  *)
(*   inbox     Seeked events in the ShellInbox, tagged with the generation *)
(*             captured by open()                                          *)
(*   attached  the element is in the document                              *)
(*                                                                         *)
(* Detach is processed as MediaSignalKind::Detached: close() drops the     *)
(* session (and its unread controls) and halt() moves gen on. Re-attaching *)
(* does not reload (attach_custom_widgets skips known media), so play()    *)
(* refetches and opens a new session with a new generation.                *)
(*                                                                         *)
(* Fixed = TRUE models close() resetting seeks.                            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, TLC

CONSTANTS Fixed, MaxGen, MaxSeeks

VARIABLES gen, feed, seeks, nonzero, controls, inbox, attached

vars == <<gen, feed, seeks, nonzero, controls, inbox, attached>>

TypeOK ==
  /\ gen \in 1..MaxGen
  /\ feed \in BOOLEAN
  /\ seeks \in 0..MaxSeeks
  /\ nonzero \in BOOLEAN
  /\ controls \in 0..(MaxSeeks + 1)
  /\ inbox \in Seq(1..MaxGen)
  /\ attached \in BOOLEAN

Init ==
  /\ gen = 1
  /\ feed = TRUE
  /\ seeks = 0
  /\ nonzero = FALSE
  /\ controls = 0
  /\ inbox = <<>>
  /\ attached = TRUE

Seek ==
  /\ seeks < MaxSeeks
  /\ nonzero' \in BOOLEAN
  /\ IF feed
       THEN /\ seeks' = seeks + 1
            /\ controls' = controls + 1
       ELSE UNCHANGED <<seeks, controls>>
  /\ UNCHANGED <<gen, feed, inbox, attached>>

SessionSeek ==
  /\ feed
  /\ controls > 0
  /\ controls' = controls - 1
  /\ inbox' = Append(inbox, gen)
  /\ UNCHANGED <<gen, feed, seeks, nonzero, attached>>

DeliverSeeked ==
  /\ inbox # <<>>
  /\ inbox' = Tail(inbox)
  /\ seeks' = IF Head(inbox) = gen /\ seeks > 0 THEN seeks - 1 ELSE seeks
  /\ UNCHANGED <<gen, feed, nonzero, controls, attached>>

Detach ==
  /\ attached
  /\ gen < MaxGen
  /\ attached' = FALSE
  /\ feed' = FALSE
  /\ controls' = 0
  /\ gen' = gen + 1
  /\ seeks' = IF Fixed THEN 0 ELSE seeks
  /\ UNCHANGED <<nonzero, inbox>>

Attach ==
  /\ ~attached
  /\ attached' = TRUE
  /\ UNCHANGED <<gen, feed, seeks, nonzero, controls, inbox>>

Play ==
  /\ attached
  /\ ~feed
  /\ gen < MaxGen
  /\ gen' = gen + 1
  /\ feed' = TRUE
  /\ controls' = IF nonzero THEN 1 ELSE 0
  /\ UNCHANGED <<seeks, nonzero, inbox, attached>>

Next == Seek \/ SessionSeek \/ DeliverSeeked \/ Detach \/ Attach \/ Play

Spec == Init /\ [][Next]_vars

LiveAcks == controls + Len(SelectSeq(inbox, LAMBDA g: g = gen))

SeeksAcknowledged == feed => seeks <= LiveAcks
=============================================================================
