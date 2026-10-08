--------------------------- MODULE ScraperSchedule ---------------------------
(***************************************************************************)
(* T5: scraper dependency scheduling for one scrape job.                  *)
(*                                                                         *)
(* Rust mapping (polymer-scraping/src/engine/mod.rs, polymer-pluginhost/  *)
(* scraper/mod.rs):                                                        *)
(*   Req[s], Opt[s]  ScraperPluginInstance::requirements / optional; each  *)
(*                   requirement is its key set (Key(k) = {k}, Any(ks)) *)
(*   Provides[s]     ScraperPluginInstance::provides; scrape results are  *)
(*                   restricted to these keys by PluginRuntime::scrape     *)
(*   Base            metadata keys not produced by a scraper (gumshoe,    *)
(*                   "package", "package.files")                           *)
(*   meta[s]         catalog metadata rows with producer "scraper:<s>"     *)
(*                   (replace_metadata in run_plugin); starts at Prior[s]  *)
(*                   from earlier jobs                                     *)
(*   Input           ScrapingEngine::input -> ScrapeInput key set          *)
(*   pending         execute_job `pending` (minus PriorCompleted steps)    *)
(*   stale           execute_job `stale`                                   *)
(*   inflight        JoinSet `tasks` spawned for Self::ready(..)           *)
(*   progressed      execute_job `progressed`                              *)
(*   phase           "loop" = top of execute_job loop, "join" = draining  *)
(*                   tasks.join_next, "done" = after `break`               *)
(*   attempts[s]     calls of ScrapingEngine::attempt for s                *)
(*   completed[s]    attempts that returned Ok(true) (step Completed)      *)
(*                                                                         *)
(* Actions: ExecuteJobRound / ExecuteJobBreak = one iteration of the       *)
(* execute_job loop (input, eligible, ready, spawn); Attempt = attempt +  *)
(* run_plugin with outcome "error" (plugin error/timeout/refresh failure, *)
(* no metadata write), "empty" (Ok(0): replace_metadata with no rows),    *)
(* "ok" (Ok(n>0): replace_metadata with a nonempty subset of Provides);  *)
(* JoinNext = the join_next loop finishing and `stale.clear()`.           *)
(* All tasks of a round read the same input snapshot and replace_metadata *)
(* is per-producer, so task interleaving is modelled by Attempt order.    *)
(* Abstracted: JobStore/catalog I/O errors, GameRemoved, metadata values  *)
(* (only key presence), registry changes during a job.                    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Scrapers, Keys, Base, Req, Opt, Provides, Outcomes, Prior,
          PriorCompleted

ASSUME Base \subseteq Keys
ASSUME Req \in [Scrapers -> SUBSET SUBSET Keys]
ASSUME Opt \in [Scrapers -> SUBSET SUBSET Keys]
ASSUME Provides \in [Scrapers -> SUBSET Keys]
ASSUME Outcomes \in [Scrapers -> (SUBSET {"error", "empty", "ok"}) \ {{}}]
ASSUME Prior \in [Scrapers -> SUBSET Keys]
ASSUME PriorCompleted \subseteq Scrapers

VARIABLES meta, pending, stale, inflight, progressed, phase, attempts, completed

vars == <<meta, pending, stale, inflight, progressed, phase, attempts, completed>>

Input == Base \cup UNION {meta[s] : s \in Scrapers}

Satisfied(r, I) == r \cap I # {}

Accepts(s, I) == \A r \in Req[s] : Satisfied(r, I)

Awaits(s, I, k) == \E r \in Req[s] \cup Opt[s] : ~Satisfied(r, I) /\ k \in r

Eligible(I) == {s \in pending \ stale : Accepts(s, I)}

Ready(E, I) ==
  LET r == {s \in E : ~\E o \in E \ {s} : \E k \in Provides[o] : Awaits(s, I, k)}
  IN IF r = {} THEN E ELSE r

TypeOK ==
  /\ meta \in [Scrapers -> SUBSET Keys]
  /\ pending \subseteq Scrapers
  /\ stale \subseteq Scrapers
  /\ inflight \subseteq Scrapers
  /\ progressed \in BOOLEAN
  /\ phase \in {"loop", "join", "done"}
  /\ attempts \in [Scrapers -> Nat]
  /\ completed \in [Scrapers -> Nat]

Init ==
  /\ meta = Prior
  /\ pending = Scrapers \ PriorCompleted
  /\ stale = {}
  /\ inflight = {}
  /\ progressed = FALSE
  /\ phase = "loop"
  /\ attempts = [s \in Scrapers |-> 0]
  /\ completed = [s \in Scrapers |-> 0]

ExecuteJobRound ==
  /\ phase = "loop"
  /\ Eligible(Input) # {}
  /\ LET ready == Ready(Eligible(Input), Input)
     IN /\ inflight' = ready
        /\ stale' = stale \cup ready
  /\ progressed' = FALSE
  /\ phase' = "join"
  /\ UNCHANGED <<meta, pending, attempts, completed>>

ExecuteJobBreak ==
  /\ phase = "loop"
  /\ Eligible(Input) = {}
  /\ phase' = "done"
  /\ UNCHANGED <<meta, pending, stale, inflight, progressed, attempts, completed>>

Attempt(s) ==
  /\ phase = "join"
  /\ s \in inflight
  /\ inflight' = inflight \ {s}
  /\ attempts' = [attempts EXCEPT ![s] = @ + 1]
  /\ \/ /\ "error" \in Outcomes[s]
        /\ UNCHANGED <<meta, pending, progressed, completed>>
     \/ /\ "empty" \in Outcomes[s]
        /\ meta' = [meta EXCEPT ![s] = {}]
        /\ UNCHANGED <<pending, progressed, completed>>
     \/ /\ "ok" \in Outcomes[s]
        /\ \E rows \in (SUBSET Provides[s]) \ {{}} :
             meta' = [meta EXCEPT ![s] = rows]
        /\ pending' = pending \ {s}
        /\ progressed' = TRUE
        /\ completed' = [completed EXCEPT ![s] = @ + 1]
  /\ UNCHANGED <<stale, phase>>

JoinNext ==
  /\ phase = "join"
  /\ inflight = {}
  /\ stale' = IF progressed THEN {} ELSE stale
  /\ phase' = "loop"
  /\ UNCHANGED <<meta, pending, inflight, progressed, attempts, completed>>

Finished ==
  /\ phase = "done"
  /\ UNCHANGED vars

Next ==
  \/ ExecuteJobRound
  \/ ExecuteJobBreak
  \/ \E s \in Scrapers : Attempt(s)
  \/ JoinNext
  \/ Finished

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(ExecuteJobRound)
  /\ WF_vars(ExecuteJobBreak)
  /\ \A s \in Scrapers : WF_vars(Attempt(s))
  /\ WF_vars(JoinNext)

CompletesAtMostOnce ==
  \A s \in Scrapers : completed[s] + (IF s \in PriorCompleted THEN 1 ELSE 0) <= 1

AttemptsAtMostOnce == \A s \in Scrapers : attempts[s] <= 1

DoneCoversAccepted ==
  phase = "done" =>
    \A s \in Scrapers : Accepts(s, Input) => (s \notin pending \/ attempts[s] > 0)

Terminates == <>(phase = "done")

EventuallyRuns ==
  \A s \in Scrapers \ PriorCompleted :
    [](Accepts(s, Input) => <>(attempts[s] > 0))
=============================================================================
