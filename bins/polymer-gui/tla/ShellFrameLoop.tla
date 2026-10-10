--------------------------- MODULE ShellFrameLoop ---------------------------
(***************************************************************************)
(* S5: shell thread frame loop.                                            *)
(*                                                                         *)
(* Rust mapping (bins/polymer-gui/src/shell):                              *)
(*   pc             position in ShellHost::poll: "sleeping" = returned     *)
(*                  Poll::Pending, "receive" = receive, "eventloop" =      *)
(*                  runtime.poll_event_loop, "paint" = paint up to         *)
(*                  produce_frame, "layout" = runtime.publish_layout       *)
(*   woken          the task's Waker was signalled (command channel,       *)
(*                  Deno timer, or frame_timer)                            *)
(*   cmds           queued ShellCommand kinds (Wake, Invalidate, Input)    *)
(*   paintRequested ShellHost.paint_requested                              *)
(*   dirty          ShellDocument.dirty (document/mod.rs)                  *)
(*   arrived        ShellDocument take_arrivals() would return true        *)
(*   animating      BaseDocument::is_animating()                           *)
(*   scriptReady    Deno has a ready task (timer / rAF setTimeout)         *)
(*   frameAge       now - ShellHost.last_frame, capped                     *)
(*   animAge        now - ShellDocument.last_animation_frame, capped       *)
(*   pubAge         now - time of the last published ShellEvent::Frame     *)
(*   timerArmed     ShellHost.frame_timer reset to a future due            *)
(*   spacingOk      history: every published frame was >= MinFrameSpacing  *)
(*                  after the previous one                                 *)
(*                                                                         *)
(* Time is abstract ticks; MinFrameSpacing = MIN_FRAME_SPACING,            *)
(* AnimInterval = ANIMATION_FRAME_INTERVAL. TimeInTurn lets time pass      *)
(* while a poll turn runs script / produce_frame. paint_due, needs_paint,  *)
(* animation_frame_due and produce_frame's early-out are modelled          *)
(* exactly; script effects (Input dispatch, Deno tasks, publish_layout     *)
(* syncLayout) are a nondeterministic ScriptEffects step. Shutdown,        *)
(* reload, resize, ValidateLaunch are out of scope (see S4).               *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS MinFrameSpacing, AnimInterval, TimeInTurn

ASSUME 1 <= MinFrameSpacing /\ MinFrameSpacing <= AnimInterval
ASSUME TimeInTurn \in BOOLEAN

Cap == AnimInterval
CommandKinds == {"Wake", "Invalidate", "Input"}

VARIABLES pc, woken, cmds, paintRequested, dirty, arrived, animating,
          scriptReady, frameAge, animAge, pubAge, timerArmed,
          spacingOk

vars == <<pc, woken, cmds, paintRequested, dirty, arrived, animating,
          scriptReady, frameAge, animAge, pubAge, timerArmed, spacingOk>>

TypeOK ==
  /\ pc \in {"sleeping", "receive", "eventloop", "paint", "layout"}
  /\ cmds \subseteq CommandKinds
  /\ frameAge \in 0..Cap /\ animAge \in 0..Cap /\ pubAge \in 0..Cap

AnimDue == animating /\ animAge >= AnimInterval
NeedsPaint == dirty \/ AnimDue
Changed == paintRequested \/ NeedsPaint
AnyDue == Changed \/ animating
DueNow == (Changed /\ frameAge >= MinFrameSpacing) \/ AnimDue
Produces == arrived \/ dirty \/ AnimDue

Init ==
  /\ pc = "receive" /\ woken = FALSE /\ cmds = {}
  /\ paintRequested = TRUE
  /\ dirty \in BOOLEAN /\ arrived = FALSE /\ animating \in BOOLEAN
  /\ scriptReady \in BOOLEAN
  /\ frameAge = MinFrameSpacing /\ animAge = 0 /\ pubAge = Cap
  /\ timerArmed = FALSE /\ spacingOk = TRUE

Inc(x) == IF x < Cap THEN x + 1 ELSE x

\* Script run by Input dispatch, Deno tasks, or publish_layout's syncLayout.
ScriptEffects(wasDirty) ==
  \E mutates \in BOOLEAN, anim \in BOOLEAN :
    /\ dirty' = (wasDirty \/ mutates)
    /\ animating' = anim

-----------------------------------------------------------------------------
\* Environment

Tick ==
  /\ TimeInTurn \/ pc = "sleeping"
  /\ frameAge' = Inc(frameAge) /\ animAge' = Inc(animAge) /\ pubAge' = Inc(pubAge)
  /\ UNCHANGED <<pc, woken, cmds, paintRequested, dirty, arrived, animating,
                 scriptReady, timerArmed, spacingOk>>

SendCommand(k) ==
  /\ k \notin cmds
  /\ cmds' = cmds \cup {k} /\ woken' = TRUE
  /\ arrived' = (arrived \/ k = "Wake")
  /\ UNCHANGED <<pc, paintRequested, dirty, animating, scriptReady, frameAge,
                 animAge, pubAge, timerArmed, spacingOk>>

ScriptTimerDue ==
  /\ ~scriptReady
  /\ scriptReady' = TRUE /\ woken' = TRUE
  /\ UNCHANGED <<pc, cmds, paintRequested, dirty, arrived, animating, frameAge,
                 animAge, pubAge, timerArmed, spacingOk>>

TimerFires ==
  /\ timerArmed /\ DueNow
  /\ timerArmed' = FALSE /\ woken' = TRUE
  /\ UNCHANGED <<pc, cmds, paintRequested, dirty, arrived, animating,
                 scriptReady, frameAge, animAge, pubAge, spacingOk>>

-----------------------------------------------------------------------------
\* Shell thread

Wakeup ==
  /\ pc = "sleeping" /\ woken
  /\ pc' = "receive" /\ woken' = FALSE
  /\ UNCHANGED <<cmds, paintRequested, dirty, arrived, animating, scriptReady,
                 frameAge, animAge, pubAge, timerArmed, spacingOk>>

Receive ==
  /\ pc = "receive"
  /\ cmds' = {}
  /\ paintRequested' = (paintRequested \/ "Wake" \in cmds)
  /\ IF "Input" \in cmds
       THEN ScriptEffects(dirty \/ "Invalidate" \in cmds)
       ELSE /\ dirty' = (dirty \/ "Invalidate" \in cmds)
            /\ UNCHANGED animating
  /\ pc' = "eventloop"
  /\ UNCHANGED <<woken, arrived, scriptReady, frameAge, animAge, pubAge,
                 timerArmed, spacingOk>>

PollEventLoop ==
  /\ pc = "eventloop"
  /\ IF scriptReady
       THEN /\ ScriptEffects(dirty)
            /\ \E requeue \in BOOLEAN :
                 /\ scriptReady' = requeue
                 /\ woken' = (woken \/ requeue)
       ELSE UNCHANGED <<dirty, animating, scriptReady, woken>>
  /\ pc' = "paint"
  /\ UNCHANGED <<cmds, paintRequested, arrived, frameAge, animAge, pubAge,
                 timerArmed, spacingOk>>

PaintIdle ==
  /\ pc = "paint" /\ ~DueNow
  /\ pc' = "sleeping"
  /\ timerArmed' = (timerArmed \/ AnyDue)
  /\ UNCHANGED <<woken, cmds, paintRequested, dirty, arrived, animating,
                 scriptReady, frameAge, animAge, pubAge, spacingOk>>

Paint ==
  /\ pc = "paint" /\ DueNow
  /\ paintRequested' = FALSE /\ frameAge' = 0
  /\ IF Produces
       THEN /\ animAge' = 0 /\ arrived' = FALSE /\ pubAge' = 0 /\ dirty' = FALSE
            /\ spacingOk' = (spacingOk /\ pubAge >= MinFrameSpacing)
       ELSE UNCHANGED <<animAge, arrived, pubAge, dirty, spacingOk>>
  /\ pc' = "layout"
  /\ UNCHANGED <<woken, cmds, animating, scriptReady, timerArmed>>

PublishLayout ==
  /\ pc = "layout"
  /\ ScriptEffects(dirty)
  /\ pc' = "sleeping" /\ woken' = TRUE
  /\ UNCHANGED <<cmds, paintRequested, arrived, scriptReady, frameAge,
                 animAge, pubAge, timerArmed, spacingOk>>

-----------------------------------------------------------------------------

Next ==
  \/ Tick \/ ScriptTimerDue \/ TimerFires \/ \E k \in CommandKinds : SendCommand(k)
  \/ Wakeup \/ Receive \/ PollEventLoop \/ PaintIdle \/ Paint \/ PublishLayout

Spec ==
  /\ Init /\ [][Next]_vars
  /\ SF_vars(Tick) /\ WF_vars(TimerFires)
  /\ WF_vars(Wakeup) /\ WF_vars(Receive) /\ WF_vars(PollEventLoop)
  /\ WF_vars(PaintIdle) /\ WF_vars(Paint) /\ WF_vars(PublishLayout)

-----------------------------------------------------------------------------
\* Properties

FrameSpacing == spacingOk

TurnTerminates == (pc \in {"receive", "eventloop", "paint", "layout"}) ~> (pc = "sleeping")

ChangePainted == dirty ~> ~dirty

PaintRequestServed == paintRequested ~> ~paintRequested

AnimationPainted == AnimDue ~> (animAge = 0 \/ ~animating)
=============================================================================
