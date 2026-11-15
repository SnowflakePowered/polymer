--------------------------- MODULE QuitLifecycle ---------------------------
(***************************************************************************)
(* Quick Resume quit racing the emulator process exit.                     *)
(*                                                                         *)
(* Rust mapping (src/app/actor/state.rs, src/app/coordinator_commands.rs,  *)
(*               src/app/coordinator_game.rs, src/app/coordinator_input.rs,*)
(*               src/app/actor/mod.rs):                                    *)
(*   active    GameHost::is_active(token) for the quitting game            *)
(*   process   the emulator process: "running" or "exited"                 *)
(*   quitting  ShellCoordinator::quitting                                  *)
(*   close     the StateActor Close future: "idle", "awaiting" = awaiting  *)
(*             the Quit reply after begin_quit, "done"                     *)
(*   reply     the controller session's onQuit reply: "none", "state"      *)
(*             (Ok(Some(bytes))), "empty" (Ok(None)), "error" (Err, which  *)
(*             includes the session being dropped by finish_active_game)   *)
(*   outcome   what finish_quit did to the QuickResumeStore entry:         *)
(*             "untouched", "committed", "removed", "reported" (removed    *)
(*             and a quickresumeerror instance event emitted)              *)
(*                                                                         *)
(* QuickResumeStore::take already deleted any earlier entry at launch, so  *)
(* "untouched" leaves no entry: only a "state" reply loses data.           *)
(*                                                                         *)
(* Fixed = TRUE models resuming Close with GuiState::with instead of       *)
(* with_active. close_game is already token-guarded: request_close returns *)
(* FALSE once finish_active_game has run.                                  *)
(***************************************************************************)
EXTENDS TLC

CONSTANTS Fixed

VARIABLES active, process, quitting, close, reply, outcome

vars == <<active, process, quitting, close, reply, outcome>>

TypeOK ==
  /\ active \in BOOLEAN
  /\ process \in {"running", "exited"}
  /\ quitting \in BOOLEAN
  /\ close \in {"idle", "awaiting", "done"}
  /\ reply \in {"none", "state", "empty", "error"}
  /\ outcome \in {"untouched", "committed", "removed", "reported"}

Init ==
  /\ active = TRUE
  /\ process = "running"
  /\ quitting = FALSE
  /\ close = "idle"
  /\ reply = "none"
  /\ outcome = "untouched"

BeginQuit ==
  /\ close = "idle"
  /\ active
  /\ ~quitting
  /\ quitting' = TRUE
  /\ close' = "awaiting"
  /\ UNCHANGED <<active, process, reply, outcome>>

PluginReply ==
  /\ close = "awaiting"
  /\ reply = "none"
  /\ reply' \in IF active THEN {"state", "empty", "error"} ELSE {"error"}
  /\ UNCHANGED <<active, process, quitting, close, outcome>>

ProcessExit ==
  /\ process = "running"
  /\ process' = "exited"
  /\ UNCHANGED <<active, quitting, close, reply, outcome>>

PollTarget ==
  /\ active
  /\ process = "exited"
  /\ active' = FALSE
  /\ quitting' = FALSE
  /\ UNCHANGED <<process, close, reply, outcome>>

Settled ==
  CASE reply = "state" -> "committed"
    [] reply = "empty" -> "removed"
    [] reply = "error" -> "reported"

ResumeClose ==
  /\ close = "awaiting"
  /\ reply # "none"
  /\ close' = "done"
  /\ IF active \/ Fixed
       THEN /\ outcome' = Settled
            /\ quitting' = FALSE
            /\ active' = FALSE
            /\ process' = "exited"
       ELSE UNCHANGED <<outcome, quitting, active, process>>
  /\ UNCHANGED reply

Done ==
  /\ close # "awaiting"
  /\ ~active
  /\ UNCHANGED vars

Next == BeginQuit \/ PluginReply \/ ProcessExit \/ PollTarget \/ ResumeClose \/ Done

Spec == Init /\ [][Next]_vars
        /\ WF_vars(BeginQuit) /\ WF_vars(PluginReply) /\ WF_vars(PollTarget)
        /\ WF_vars(ResumeClose)

QuitSettles == close = "done" => outcome # "untouched"

StateKept == (close = "done" /\ reply = "state") => outcome = "committed"

QuitCompletes == <>(close # "awaiting" /\ ~active)
=============================================================================
