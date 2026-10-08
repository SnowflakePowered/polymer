--------------------------- MODULE ShellLifecycle ---------------------------
(***************************************************************************)
(* S4: shell session lifecycle across activate / reload / exit.            *)
(*                                                                         *)
(* Rust mapping (bins/polymer-gui/src):                                    *)
(*   slot          shell/mod.rs ShellSession.slot                          *)
(*                 (Awaiting / Active / Disabled)                          *)
(*   activeGen     which spawned shell thread the Active slot owns         *)
(*   gen           number of shell threads spawned by ShellHost::spawn     *)
(*   reloadReq     ActiveShell.reload_requested (set by drain)             *)
(*   win           window-thread position:                                 *)
(*                   "loading"    theme load/reload_document awaited in    *)
(*                                application.rs run_async_tasks /         *)
(*                                app.rs reload_shell                      *)
(*                   "awaitStart" activate awaiting `started`              *)
(*                 Both run inside main.rs runtime.block_on(               *)
(*                 run_async_tasks()), so the actix LocalSet (actor        *)
(*                 futures such as Handler<Launch>) does not run then.     *)
(*                   "joining"    ActiveShell::drop blocked in             *)
(*                                thread.join() (window thread blocked)    *)
(*                   "idle"       coordinator available to actors          *)
(*                   "exited"     ShellSession dropped, app exited         *)
(*   joinFor       why the join runs (take_reload or exit)                 *)
(*   exitReq       window closed; coordinator / ShellSession dropped       *)
(*   shell[g]      thread g: "starting" = ShellHost::start (document       *)
(*                 scripts), "running" = poll loop, "looping" = script     *)
(*                 never yields, "exited" = run returned                   *)
(*   startOk[g]    the `ready` oneshot (StartReply) of thread g            *)
(*   cmds[g]       ShellChannel / UnboundedReceiver<ShellCommand> of g     *)
(*   evs[g]        ShellOutbox.events / ActiveShell.events of g            *)
(*   reported[g]   ShellHost.reload_reported                               *)
(*   launch*       emulator.rs Handler<Launch>: validate_launch, then      *)
(*                 state.launch with the reply (unwrap_or(None))           *)
(*   validatedBy   shell generation that answered ValidateLaunch,          *)
(*                 0 = resolved None (no shell / reply dropped)            *)
(*   launchedUnder shell generation Active when state.launch ran, 0=none   *)
(*   staleSettle   history: an InputSettled from another generation was    *)
(*                 applied by drain_shell -> InputHold::settle             *)
(*                                                                         *)
(* Actions: Activate / ActivateStarted / ActivateFailed = activate;        *)
(* SourceMissing = activate_shell(None) -> disable; Drain = drain;         *)
(* TakeReload = take_reload (+ ActiveShell::drop sending Shutdown);        *)
(* JoinDone = thread.join() returning; RequestExit = ShellSession drop;    *)
(* Start* = ShellHost::run/start; Receive = receive/handle;                *)
(* ScriptRequestsReload = poll_event_loop + report_reload;                 *)
(* ScriptLoops = a script task that never returns to poll_event_loop.      *)
(* Abstracted: frames, ShellUpdated wake coalescing (S3), input payloads,  *)
(* coalescing (S2), held-input timeouts (S1), exit while not idle.         *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS MaxGen, MaxInputs, MaxLoops

Gens == 1..MaxGen

VARIABLES slot, activeGen, gen, reloadReq, win, joinFor, exitReq,
          shell, startOk, cmds, evs, reported, loops, inputs,
          launch, launchGen, validatedBy, launchedUnder, staleSettle

vars == <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq,
          shell, startOk, cmds, evs, reported, loops, inputs,
          launch, launchGen, validatedBy, launchedUnder, staleSettle>>

Cmd(k) == [kind |-> k]
Ev(k, g) == [kind |-> k, gen |-> g]

TypeOK ==
  /\ slot \in {"Awaiting", "Active", "Disabled"}
  /\ activeGen \in 0..MaxGen /\ gen \in 0..MaxGen
  /\ win \in {"loading", "awaitStart", "joining", "idle", "exited"}
  /\ joinFor \in {"none", "reload", "exit"}
  /\ shell \in [Gens -> {"none", "starting", "running", "looping", "exited"}]
  /\ startOk \in [Gens -> {"pending", "ok", "err"}]
  /\ launch \in {"none", "validating", "launching", "done"}

Init ==
  /\ slot = "Awaiting" /\ activeGen = 0 /\ gen = 0 /\ reloadReq = FALSE
  /\ win = "loading" /\ joinFor = "none" /\ exitReq = FALSE
  /\ shell = [g \in Gens |-> "none"]
  /\ startOk = [g \in Gens |-> "pending"]
  /\ cmds = [g \in Gens |-> <<>>]
  /\ evs = [g \in Gens |-> <<>>]
  /\ reported = [g \in Gens |-> FALSE]
  /\ loops = 0 /\ inputs = 0
  /\ launch = "none" /\ launchGen = 0 /\ validatedBy = 0 /\ launchedUnder = 0
  /\ staleSettle = FALSE

Shells == <<shell, startOk, cmds, evs, reported, loops>>
Launches == <<launch, launchGen, validatedBy, launchedUnder>>

-----------------------------------------------------------------------------
\* Window thread

Activate ==
  /\ win = "loading" /\ slot = "Awaiting" /\ gen < MaxGen
  /\ gen' = gen + 1
  /\ shell' = [shell EXCEPT ![gen + 1] = "starting"]
  /\ slot' = "Disabled"
  /\ win' = "awaitStart"
  /\ UNCHANGED <<activeGen, reloadReq, joinFor, exitReq, startOk, cmds, evs,
                 reported, loops, inputs, Launches, staleSettle>>

SourceMissing ==
  /\ win = "loading"
  /\ slot' = "Disabled" /\ win' = "idle"
  /\ UNCHANGED <<activeGen, gen, reloadReq, joinFor, exitReq, Shells, inputs,
                 Launches, staleSettle>>

ActivateStarted ==
  /\ win = "awaitStart" /\ startOk[gen] = "ok"
  /\ slot' = "Active" /\ activeGen' = gen /\ reloadReq' = FALSE
  /\ win' = "idle"
  /\ UNCHANGED <<gen, joinFor, exitReq, Shells, inputs, Launches, staleSettle>>

ActivateFailed ==
  /\ win = "awaitStart" /\ startOk[gen] = "err" /\ shell[gen] = "exited"
  /\ slot' = "Disabled" /\ win' = "idle"
  /\ UNCHANGED <<activeGen, gen, reloadReq, joinFor, exitReq, Shells, inputs,
                 Launches, staleSettle>>

Drain ==
  /\ win = "idle" /\ slot = "Active" /\ evs[activeGen] # <<>>
  /\ LET e == Head(evs[activeGen]) IN
       /\ reloadReq' = (reloadReq \/ e.kind = "Reload")
       /\ staleSettle' = (staleSettle \/ (e.kind = "Settled" /\ e.gen # activeGen))
  /\ evs' = [evs EXCEPT ![activeGen] = Tail(@)]
  /\ UNCHANGED <<slot, activeGen, gen, win, joinFor, exitReq, shell, startOk,
                 cmds, reported, loops, inputs, Launches>>

SendInput ==
  /\ win = "idle" /\ slot = "Active" /\ inputs < MaxInputs
  /\ inputs' = inputs + 1
  /\ cmds' = [cmds EXCEPT ![activeGen] = Append(@, Cmd("Input"))]
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, shell,
                 startOk, evs, reported, loops, Launches, staleSettle>>

TakeReload ==
  /\ win = "idle" /\ slot = "Active" /\ reloadReq
  /\ cmds' = [cmds EXCEPT ![activeGen] = Append(@, Cmd("Shutdown"))]
  /\ slot' = "Awaiting" /\ reloadReq' = FALSE
  /\ win' = "joining" /\ joinFor' = "reload"
  /\ UNCHANGED <<activeGen, gen, exitReq, shell, startOk, evs, reported, loops,
                 inputs, Launches, staleSettle>>

RequestExit ==
  /\ win = "idle" /\ ~exitReq
  /\ exitReq' = TRUE
  /\ IF slot = "Active"
       THEN /\ cmds' = [cmds EXCEPT ![activeGen] = Append(@, Cmd("Shutdown"))]
            /\ win' = "joining" /\ joinFor' = "exit"
       ELSE /\ win' = "exited"
            /\ UNCHANGED <<cmds, joinFor>>
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, shell, startOk, evs, reported,
                 loops, inputs, Launches, staleSettle>>

JoinDone ==
  /\ win = "joining" /\ shell[activeGen] = "exited"
  /\ win' = IF joinFor = "reload" THEN "loading" ELSE "exited"
  /\ activeGen' = IF joinFor = "reload" THEN 0 ELSE activeGen
  /\ joinFor' = "none"
  /\ UNCHANGED <<slot, gen, reloadReq, exitReq, Shells, inputs, Launches,
                 staleSettle>>

Finished ==
  /\ win = "exited"
  /\ UNCHANGED vars

-----------------------------------------------------------------------------
\* Launch handler (actor future polled by system.block_on, i.e. only while the
\* window thread is idle)

LaunchBegin ==
  /\ launch = "none" /\ win = "idle"
  /\ CASE slot = "Active" ->
            /\ launch' = "validating" /\ launchGen' = activeGen
            /\ cmds' = [cmds EXCEPT ![activeGen] = Append(@, Cmd("Validate"))]
            /\ UNCHANGED validatedBy
       [] OTHER ->
            /\ launch' = "launching" /\ validatedBy' = 0
            /\ UNCHANGED <<cmds, launchGen>>
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, shell,
                 startOk, evs, reported, loops, inputs, launchedUnder, staleSettle>>

LaunchApply ==
  /\ launch = "launching" /\ win = "idle"
  /\ launch' = "done"
  /\ launchedUnder' = IF slot = "Active" THEN activeGen ELSE 0
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, Shells,
                 inputs, launchGen, validatedBy, staleSettle>>

-----------------------------------------------------------------------------
\* Shell thread g

StartOk(g) ==
  /\ shell[g] = "starting"
  /\ shell' = [shell EXCEPT ![g] = "running"]
  /\ startOk' = [startOk EXCEPT ![g] = "ok"]
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, cmds,
                 evs, reported, loops, inputs, Launches, staleSettle>>

StartFail(g) ==
  /\ shell[g] = "starting"
  /\ shell' = [shell EXCEPT ![g] = "exited"]
  /\ startOk' = [startOk EXCEPT ![g] = "err"]
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, cmds,
                 evs, reported, loops, inputs, Launches, staleSettle>>

StartLoops(g) ==
  /\ shell[g] = "starting" /\ loops < MaxLoops
  /\ shell' = [shell EXCEPT ![g] = "looping"]
  /\ loops' = loops + 1
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, startOk,
                 cmds, evs, reported, inputs, Launches, staleSettle>>

DropPendingValidate(g) ==
  IF launch = "validating" /\ launchGen = g
    THEN /\ launch' = "launching" /\ validatedBy' = 0
    ELSE UNCHANGED <<launch, validatedBy>>

Receive(g) ==
  /\ shell[g] = "running" /\ cmds[g] # <<>>
  /\ cmds' = [cmds EXCEPT ![g] = Tail(@)]
  /\ CASE Head(cmds[g]).kind = "Shutdown" ->
            /\ shell' = [shell EXCEPT ![g] = "exited"]
            /\ DropPendingValidate(g)
            /\ UNCHANGED evs
       [] Head(cmds[g]).kind = "Input" ->
            /\ evs' = [evs EXCEPT ![g] = Append(@, Ev("Settled", g))]
            /\ UNCHANGED <<shell, launch, validatedBy>>
       [] Head(cmds[g]).kind = "Validate" ->
            /\ launch' = "launching" /\ validatedBy' = g
            /\ UNCHANGED <<shell, evs>>
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, startOk,
                 reported, loops, inputs, launchGen, launchedUnder, staleSettle>>

ScriptRequestsReload(g) ==
  /\ shell[g] = "running" /\ ~reported[g]
  /\ reported' = [reported EXCEPT ![g] = TRUE]
  /\ evs' = [evs EXCEPT ![g] = Append(@, Ev("Reload", g))]
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, shell,
                 startOk, cmds, loops, inputs, Launches, staleSettle>>

ScriptLoops(g) ==
  /\ shell[g] = "running" /\ loops < MaxLoops
  /\ shell' = [shell EXCEPT ![g] = "looping"]
  /\ loops' = loops + 1
  /\ UNCHANGED <<slot, activeGen, gen, reloadReq, win, joinFor, exitReq, startOk,
                 cmds, evs, reported, inputs, Launches, staleSettle>>

-----------------------------------------------------------------------------

Window == Activate \/ SourceMissing \/ ActivateStarted \/ ActivateFailed
          \/ Drain \/ SendInput \/ TakeReload \/ RequestExit \/ JoinDone

Next ==
  \/ Window \/ LaunchBegin \/ LaunchApply \/ Finished
  \/ \E g \in Gens : \/ StartOk(g) \/ StartFail(g) \/ StartLoops(g)
                     \/ Receive(g) \/ ScriptRequestsReload(g) \/ ScriptLoops(g)

Fairness ==
  /\ WF_vars(Activate \/ SourceMissing)
  /\ WF_vars(ActivateStarted) /\ WF_vars(ActivateFailed)
  /\ WF_vars(Drain) /\ WF_vars(TakeReload) /\ WF_vars(JoinDone)
  /\ WF_vars(LaunchApply)
  /\ \A g \in Gens : WF_vars(StartOk(g) \/ StartFail(g)) /\ WF_vars(Receive(g))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* Properties

NoStaleSettle == ~staleSettle

LaunchValidatedByCurrentShell ==
  (launch = "done" /\ launchedUnder # 0) => validatedBy = launchedUnder

NoUnvalidatedLaunchUnderShell ==
  (launch = "done" /\ validatedBy = 0) => launchedUnder = 0

ReloadCompletes ==
  (win = "joining" /\ joinFor = "reload") ~> (win = "idle")

ExitCompletes == exitReq ~> (win = "exited")

LaunchResolves ==
  (launch \in {"validating", "launching"})
    ~> (launch = "done" \/ win = "exited")
=============================================================================
