---------------------------- MODULE HeldInput ----------------------------
(***************************************************************************)
(* S1 held input protocol, with S2 pointer-move coalescing folded in.      *)
(*                                                                         *)
(* Rust mapping:                                                           *)
(*   held          app/held_input.rs InputHold.queue (Held entries;        *)
(*                 .set/.claimed/.over = Held.outcome, .expired =          *)
(*                 now - arrived >= HOLD_TIMEOUT, .wasSet = flag_was_set)  *)
(*   nextSeq       InputHold.next_sequence                                 *)
(*   mask          input.rs InputState.mouse_buttons (set_mouse_button)    *)
(*   priority      InputState.priority (replace_priority)                  *)
(*   shellUp       shell/mod.rs ShellSession.slot is Active (is_active)    *)
(*   reloadPending ActiveShell.reload_requested (drain, reload_pending)    *)
(*   chan          ShellChannel / ShellHost.commands (ShellCommand::Input) *)
(*   batch         shell/host/mod.rs receive(): batch being dispatched     *)
(*   replaced      receive(): `replaced` (superseded pointer moves)        *)
(*   outbox        ShellOutbox.events / ActiveShell.events (ShellEvent)    *)
(*   reloadReported ShellHost.reload_reported (report_reload)              *)
(*   gameKey       input.rs keyboard.sample / HostInputEvent::Key          *)
(*                 (route_game_key); cleared by HostInputEvent::FocusLost  *)
(*   gameBtn       polymer-renderer PointerRoutingState.buttons and the    *)
(*                 game's view of it (route_game_pointer)                  *)
(*   phys, physBtn physical keyboard / mouse state (winit events)          *)
(*   keyCycle, btnCycle, gPress, gRel, gbPress, gbRel                      *)
(*                 history: press cycle numbers delivered to the game      *)
(*   relCount, shellOutcome, settledLog                                    *)
(*                 history: releases per sequence, outcomes produced by    *)
(*                 dispatch, sequences settled by the current shell        *)
(*   Claims        values script may return as InputOutcome.claimed        *)
(*   ReloadResults whether activate_shell succeeds after take_reload       *)
(*                                                                         *)
(* Actions:                                                                *)
(*   ShellKeyboard        coordinator_input.rs shell_keyboard              *)
(*   ShellMouse           handle_mouse_button/handle_cursor_moved ->       *)
(*                        shell_mouse                                      *)
(*   SetInputPriority     set_input_priority (prepare_priority_change,     *)
(*                        cancel_pointer_connection)                       *)
(*   Expire               time passing HOLD_TIMEOUT for the oldest entry   *)
(*   ReleaseHeldInput     one iteration of release_held_input /            *)
(*                        InputHold::release                               *)
(*   DrainShell           coordinator_presentation.rs drain_shell          *)
(*                        (ShellSession::drain + InputHold::settle)        *)
(*   ReloadShell          app.rs reload_shell: take_reload, drop of        *)
(*                        ActiveShell (Shutdown + join), activate_shell    *)
(*                        succeeding or failing (disable)                  *)
(*   Receive              shell/host/mod.rs receive(): poll_recv loop      *)
(*   Dispatch             receive(): one batch element (coalesce or        *)
(*                        dispatch + InputSettled for replaced and self)   *)
(*   ReportReload         report_reload                                    *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Keys, Buttons, MaxInputs, MaxMoves, MaxFlips, MaxReloads, ReloadResults, Claims

ASSUME ReloadResults \subseteq BOOLEAN /\ ReloadResults # {}
ASSUME Claims \subseteq BOOLEAN /\ Claims # {}

MaxSeq == MaxInputs + MaxMoves
Seqs == 1..MaxSeq
Ids == Keys \cup Buttons
NoId == "none"

NoOutcome == [set |-> FALSE, claimed |-> FALSE, over |-> FALSE]

VARIABLES phys, physBtn, keyCycle, btnCycle, priority, shellUp,
          reloadReported, reloadPending, chan, batch, replaced, outbox,
          held, nextSeq, mask, gameKey, gameBtn, gPress, gRel, gbPress, gbRel,
          relCount, shellOutcome, settledLog, inputs, moves, flips, reloads

vars == <<phys, physBtn, keyCycle, btnCycle, priority, shellUp,
          reloadReported, reloadPending, chan, batch, replaced, outbox,
          held, nextSeq, mask, gameKey, gameBtn, gPress, gRel, gbPress, gbRel,
          relCount, shellOutcome, settledLog, inputs, moves, flips, reloads>>

gameVars == <<gameKey, gameBtn, gPress, gRel, gbPress, gbRel>>
shellVars == <<shellUp, reloadReported, reloadPending, chan, batch, replaced, outbox, settledLog>>

Cmd(s, kind) == [seq |-> s, kind |-> kind]
Entry(s, kind, id, pressed, cycle, wasSet) ==
  [seq |-> s, kind |-> kind, id |-> id, pressed |-> pressed, cycle |-> cycle,
   wasSet |-> wasSet, set |-> FALSE, claimed |-> FALSE, over |-> FALSE, expired |-> FALSE]
Settled(s, o) == [type |-> "settled", seq |-> s, claimed |-> o.claimed, over |-> o.over]
ReloadEvt == [type |-> "reload", seq |-> 0, claimed |-> FALSE, over |-> FALSE]

Init ==
  /\ phys = [k \in Keys |-> FALSE]
  /\ physBtn = [b \in Buttons |-> FALSE]
  /\ keyCycle = [k \in Keys |-> 0]
  /\ btnCycle = [b \in Buttons |-> 0]
  /\ priority \in {"Game", "Ui"}
  /\ shellUp = TRUE
  /\ reloadReported = FALSE
  /\ reloadPending = FALSE
  /\ chan = <<>> /\ batch = <<>> /\ replaced = <<>> /\ outbox = <<>>
  /\ held = <<>>
  /\ nextSeq = 0
  /\ mask = [b \in Buttons |-> FALSE]
  /\ gameKey = [k \in Keys |-> FALSE]
  /\ gameBtn = [b \in Buttons |-> FALSE]
  /\ gPress = [k \in Keys |-> 0] /\ gRel = [k \in Keys |-> 0]
  /\ gbPress = [b \in Buttons |-> 0] /\ gbRel = [b \in Buttons |-> 0]
  /\ relCount = [s \in Seqs |-> 0]
  /\ shellOutcome = [s \in Seqs |-> NoOutcome]
  /\ settledLog = <<>>
  /\ inputs = 0 /\ moves = 0 /\ flips = 0 /\ reloads = 0

DeliverKey(k, pressed, cycle) ==
  /\ gameKey' = [gameKey EXCEPT ![k] = pressed]
  /\ gPress' = IF pressed THEN [gPress EXCEPT ![k] = cycle] ELSE gPress
  /\ gRel' = IF pressed THEN gRel ELSE [gRel EXCEPT ![k] = cycle]
  /\ UNCHANGED <<gameBtn, gbPress, gbRel>>

DeliverButton(b, pressed, cycle) ==
  /\ gameBtn' = [gameBtn EXCEPT ![b] = pressed]
  /\ gbPress' = IF pressed THEN [gbPress EXCEPT ![b] = cycle] ELSE gbPress
  /\ gbRel' = IF pressed THEN gbRel ELSE [gbRel EXCEPT ![b] = cycle]
  /\ UNCHANGED <<gameKey, gPress, gRel>>

\* shell_keyboard
ShellKeyboard(k) ==
  /\ inputs < MaxInputs
  /\ inputs' = inputs + 1
  /\ LET pressed == ~phys[k]
         cycle == IF pressed THEN keyCycle[k] + 1 ELSE keyCycle[k]
         gameOwned == priority = "Game"
     IN /\ phys' = [phys EXCEPT ![k] = pressed]
        /\ keyCycle' = [keyCycle EXCEPT ![k] = cycle]
        /\ IF ~shellUp
             THEN /\ IF gameOwned THEN DeliverKey(k, pressed, cycle) ELSE UNCHANGED gameVars
                  /\ UNCHANGED <<chan, held, nextSeq>>
             ELSE /\ UNCHANGED gameVars
                  /\ IF gameOwned
                       THEN /\ nextSeq' = nextSeq + 1
                            /\ held' = Append(held, Entry(nextSeq + 1, "key", k, pressed, cycle, FALSE))
                            /\ chan' = Append(chan, Cmd(nextSeq + 1, "key"))
                       ELSE /\ chan' = Append(chan, Cmd(0, "key"))
                            /\ UNCHANGED <<held, nextSeq>>
  /\ UNCHANGED <<physBtn, btnCycle, priority, mask, shellUp, reloadReported, reloadPending,
                 batch, replaced, outbox, relCount, shellOutcome, settledLog, moves, flips, reloads>>

\* handle_mouse_button -> shell_mouse(Some(pressed))
ShellMouseButton(b) ==
  /\ inputs < MaxInputs
  /\ inputs' = inputs + 1
  /\ LET pressed == ~physBtn[b]
         cycle == IF pressed THEN btnCycle[b] + 1 ELSE btnCycle[b]
     IN /\ physBtn' = [physBtn EXCEPT ![b] = pressed]
        /\ btnCycle' = [btnCycle EXCEPT ![b] = cycle]
        /\ IF ~shellUp
             THEN UNCHANGED <<mask, held, nextSeq, chan>>
             ELSE /\ mask' = [mask EXCEPT ![b] = pressed]
                  /\ nextSeq' = nextSeq + 1
                  /\ held' = Append(held, Entry(nextSeq + 1, "btn", b, pressed, cycle, mask[b]))
                  /\ chan' = Append(chan, Cmd(nextSeq + 1, "btn"))
  /\ UNCHANGED <<phys, keyCycle, priority, shellUp, reloadReported, reloadPending,
                 batch, replaced, outbox, relCount, shellOutcome, settledLog, moves, flips, reloads>>
  /\ UNCHANGED gameVars

\* handle_cursor_moved -> shell_mouse("mousemove", None)
ShellMouseMove ==
  /\ moves < MaxMoves
  /\ moves' = moves + 1
  /\ IF ~shellUp
       THEN UNCHANGED <<held, nextSeq, chan>>
       ELSE /\ nextSeq' = nextSeq + 1
            /\ held' = Append(held, Entry(nextSeq + 1, "move", NoId, FALSE, 0, FALSE))
            /\ chan' = Append(chan, Cmd(nextSeq + 1, "move"))
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, mask, shellUp, reloadReported,
                 reloadPending, batch, replaced, outbox, relCount, shellOutcome, settledLog,
                 inputs, flips, reloads>>
  /\ UNCHANGED gameVars

\* set_input_priority with a different priority: cancel_pointer_connection
\* (PointerPhase::Cancel clears buttons) and prepare_priority_change (FocusLost).
SetInputPriority ==
  /\ flips < MaxFlips
  /\ flips' = flips + 1
  /\ priority' = IF priority = "Game" THEN "Ui" ELSE "Game"
  /\ gameKey' = [k \in Keys |-> FALSE]
  /\ gameBtn' = [b \in Buttons |-> FALSE]
  /\ UNCHANGED <<gPress, gRel, gbPress, gbRel>>
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, shellUp, reloadReported, reloadPending,
                 chan, batch, replaced, outbox, held, nextSeq, mask, relCount, shellOutcome,
                 settledLog, inputs, moves, reloads>>

\* Time passes HOLD_TIMEOUT for the oldest entry not yet expired (arrival order).
Expire ==
  /\ \E i \in DOMAIN held :
       /\ ~held[i].expired
       /\ \A j \in 1..(i - 1) : held[j].expired
       /\ held' = [held EXCEPT ![i].expired = TRUE]
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadReported,
                 reloadPending, chan, batch, replaced, outbox, nextSeq, mask, relCount,
                 shellOutcome, settledLog, inputs, moves, flips, reloads>>
  /\ UNCHANGED gameVars

\* release_held_input: pop the front once settled or expired and route it.
ReleaseHeldInput ==
  /\ held # <<>>
  /\ LET h == Head(held)
         claimed == h.set /\ h.claimed
         over == h.set /\ h.over
     IN /\ h.set \/ h.expired
        /\ held' = Tail(held)
        /\ relCount' = [relCount EXCEPT ![h.seq] = @ + 1]
        /\ CASE h.kind = "key" ->
                  /\ IF ~claimed /\ priority = "Game"
                       THEN DeliverKey(h.id, h.pressed, h.cycle)
                       ELSE UNCHANGED gameVars
                  /\ UNCHANGED mask
             [] h.kind = "btn" ->
                  /\ mask' = IF ~claimed THEN [mask EXCEPT ![h.id] = h.wasSet] ELSE mask
                  /\ IF ~claimed /\ over
                       THEN DeliverButton(h.id, h.pressed, h.cycle)
                       ELSE UNCHANGED gameVars
             [] h.kind = "move" ->
                  UNCHANGED <<mask, gameKey, gameBtn, gPress, gRel, gbPress, gbRel>>
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadReported,
                 reloadPending, chan, batch, replaced, outbox, nextSeq, shellOutcome,
                 settledLog, inputs, moves, flips, reloads>>

RECURSIVE ApplySettles(_, _)
ApplySettles(q, events) ==
  IF events = <<>> THEN q
  ELSE LET e == Head(events)
           q2 == IF e.type = "settled"
                   THEN [i \in DOMAIN q |->
                           IF q[i].seq = e.seq
                             THEN [q[i] EXCEPT !.set = TRUE, !.claimed = e.claimed, !.over = e.over]
                             ELSE q[i]]
                   ELSE q
       IN ApplySettles(q2, Tail(events))

\* drain_shell: ShellSession::drain then InputHold::settle per InputSettled.
DrainShell ==
  /\ shellUp
  /\ outbox # <<>>
  /\ outbox' = <<>>
  /\ held' = ApplySettles(held, outbox)
  /\ reloadPending' = (reloadPending \/ \E i \in DOMAIN outbox : outbox[i].type = "reload")
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadReported,
                 chan, batch, replaced, nextSeq, mask, relCount, shellOutcome, settledLog,
                 inputs, moves, flips, reloads>>
  /\ UNCHANGED gameVars

\* reload_shell: take_reload drops ActiveShell (Shutdown, join; its event
\* receiver is dropped with it), then activate_shell succeeds or disables.
ReloadShell ==
  /\ shellUp /\ reloadPending
  /\ reloads' = reloads + 1
  /\ shellUp' \in ReloadResults
  /\ reloadReported' = FALSE
  /\ reloadPending' = FALSE
  /\ chan' = <<>> /\ batch' = <<>> /\ replaced' = <<>> /\ outbox' = <<>>
  /\ settledLog' = <<>>
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, held, nextSeq, mask, relCount,
                 shellOutcome, inputs, moves, flips>>
  /\ UNCHANGED gameVars

\* receive(): poll_recv until Pending; anything sent later lands in the next batch.
Receive ==
  /\ shellUp
  /\ batch = <<>>
  /\ \E n \in 1..Len(chan) :
       /\ batch' = SubSeq(chan, 1, n)
       /\ chan' = SubSeq(chan, n + 1, Len(chan))
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadReported,
                 reloadPending, replaced, outbox, held, nextSeq, mask, relCount, shellOutcome,
                 settledLog, inputs, moves, flips, reloads>>
  /\ UNCHANGED gameVars

\* receive(): one element. A move followed by a move is superseded; otherwise
\* dispatch decides an outcome and InputSettled goes out for replaced + self.
Dispatch ==
  /\ shellUp
  /\ batch # <<>>
  /\ batch' = Tail(batch)
  /\ LET c == Head(batch) IN
       IF c.kind = "move" /\ Len(batch) > 1 /\ batch[2].kind = "move"
         THEN /\ replaced' = Append(replaced, c)
              /\ UNCHANGED <<outbox, shellOutcome, settledLog>>
         ELSE \E claimed \in Claims, over \in (IF c.kind = "key" THEN {FALSE} ELSE BOOLEAN) :
                LET o == [set |-> TRUE, claimed |-> claimed, over |-> over]
                    group == replaced \o (IF c.seq = 0 THEN <<>> ELSE <<c>>)
                    seqs == [i \in DOMAIN group |-> group[i].seq]
                IN /\ replaced' = <<>>
                   /\ outbox' = outbox \o [i \in DOMAIN seqs |-> Settled(seqs[i], o)]
                   /\ shellOutcome' = [s \in Seqs |->
                                         IF \E i \in DOMAIN seqs : seqs[i] = s THEN o ELSE shellOutcome[s]]
                   /\ settledLog' = settledLog \o seqs
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadReported,
                 reloadPending, chan, held, nextSeq, mask, relCount, inputs, moves, flips, reloads>>
  /\ UNCHANGED gameVars

ReportReload ==
  /\ shellUp
  /\ ~reloadReported
  /\ reloads < MaxReloads
  /\ reloadReported' = TRUE
  /\ outbox' = Append(outbox, ReloadEvt)
  /\ UNCHANGED <<phys, physBtn, keyCycle, btnCycle, priority, shellUp, reloadPending, chan,
                 batch, replaced, held, nextSeq, mask, relCount, shellOutcome, settledLog,
                 inputs, moves, flips, reloads>>
  /\ UNCHANGED gameVars

Quiescent ==
  /\ held = <<>> /\ chan = <<>> /\ batch = <<>> /\ outbox = <<>>

Finished ==
  /\ Quiescent
  /\ UNCHANGED vars

WindowNext ==
  \/ \E k \in Keys : ShellKeyboard(k)
  \/ \E b \in Buttons : ShellMouseButton(b)
  \/ ShellMouseMove
  \/ SetInputPriority
  \/ Expire
  \/ ReleaseHeldInput
  \/ DrainShell
  \/ ReloadShell

ShellNext == Receive \/ Dispatch \/ ReportReload

Next == WindowNext \/ ShellNext \/ Finished

WindowFairness ==
  /\ WF_vars(Expire)
  /\ WF_vars(ReleaseHeldInput)
  /\ WF_vars(DrainShell)
  /\ WF_vars(ReloadShell)

\* The script may stall forever: no fairness on the shell thread.
Spec == Init /\ [][Next]_vars /\ WindowFairness

\* The script always makes progress eventually.
FairSpec == Spec /\ WF_vars(Receive) /\ WF_vars(Dispatch)

-----------------------------------------------------------------------------

TypeOK ==
  /\ priority \in {"Game", "Ui"}
  /\ nextSeq \in 0..MaxSeq
  /\ relCount \in [Seqs -> 0..2]
  /\ \A i \in DOMAIN held : held[i].seq \in Seqs

\* Every held sequence is released at most once ...
ReleasedAtMostOnce == \A s \in Seqs : relCount[s] <= 1

\* ... and at least once.
EveryHeldReleased == <>[](\A s \in 1..nextSeq : relCount[s] = 1)

\* The game never receives the press of a cycle after already having
\* received that cycle's release.
ReleaseNotBeforePress ==
  [][/\ \A k \in Keys : gPress'[k] # gPress[k] => gPress'[k] > gRel[k]
     /\ \A b \in Buttons : gbPress'[b] # gbPress[b] => gbPress'[b] > gbRel[b]]_vars

KeysConverged == \A k \in Keys : ~phys[k] => ~gameKey[k]
ButtonsConverged == \A b \in Buttons : ~physBtn[b] => ~gameBtn[b]
MaskConverged == \A b \in Buttons : ~physBtn[b] => ~mask[b]

\* With all input stopped, nothing stays pressed that is physically released.
NoStuckKeys == <>[]KeysConverged
NoStuckButtons == <>[]ButtonsConverged
NoStuckMask == <>[]MaskConverged

\* S2: the current shell settles each sequence once and in order.
SettledInOrder ==
  \A i, j \in DOMAIN settledLog : i < j => settledLog[i] < settledLog[j]

\* S2: only moves are superseded, and only by an immediately following move.
OnlyConsecutiveMovesCoalesce ==
  /\ \A i \in DOMAIN replaced : replaced[i].kind = "move"
  /\ replaced # <<>> => (batch # <<>> /\ Head(batch).kind = "move")

\* S2: a settled outcome in the hold is what dispatch produced for that sequence.
NoStaleOutcome ==
  \A i \in DOMAIN held :
    held[i].set => /\ shellOutcome[held[i].seq].set
                   /\ shellOutcome[held[i].seq].claimed = held[i].claimed
                   /\ shellOutcome[held[i].seq].over = held[i].over

=============================================================================
