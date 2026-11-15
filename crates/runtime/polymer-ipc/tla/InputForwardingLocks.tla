--------------------------- MODULE InputForwardingLocks ---------------------------
(***************************************************************************)
(* Lock order of the host input listener (src/input/host/mod.rs).          *)
(*                                                                         *)
(* Locks:                                                                  *)
(*   "H"  Inner::hub, the Mutex<Option<ListenerHandle>>                    *)
(*   "F"  Inner::forwarding, the Mutex<ForwardingState>                    *)
(*   "P"  Inner::publication, the Mutex<InputPublication>                  *)
(*                                                                         *)
(* Threads and the lock programs they run:                                 *)
(*   "plugin"   InputIpcListener::activate_buttons, reached from           *)
(*              op_action_activate (polymer-pluginhost/actions/mod.rs) on  *)
(*              the controller session's own actix System thread           *)
(*              (runtime/controller_session/mod.rs). It locks H, finds the *)
(*              device, locks F, publishes the chord under P, sleeps       *)
(*              HOLD_TIME still holding H and F, then publishes the        *)
(*              release under P.                                           *)
(*   "coord"    InputIpcListener::open_raw_hid, reached from the GUI       *)
(*              coordinator thread through InputRouter::rebuild_input_hooks*)
(*              (bins/polymer-gui/src/input.rs). It locks F, then reads    *)
(*              neutrality under H.                                        *)
(*   "backend"  InputListener::on_input from an input backend thread       *)
(*              (SDL worker, Bluetooth tokio task): F, then P.             *)
(*                                                                         *)
(* Fixed = TRUE models the applied change: activate_buttons and           *)
(* open_raw_hid clone the InputHub out of H and drop H before taking F.    *)
(* activate_buttons marks the player in forwarding.holding under F, drops  *)
(* F for the sleep, then retakes F and P to publish the live state.        *)
(* open_raw_hid reads neutrality through InputHub::state under F; the hub  *)
(* dispatches listeners after dropping its topology guard, so that read    *)
(* adds no edge. No thread nests H and F and nothing sleeps under F.       *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Fixed, Runs

Threads == {"plugin", "coord", "backend"}
Locks == {"H", "F", "P"}
None == "none"

Acq(l) == [op |-> "acq", lock |-> l]
Rel(l) == [op |-> "rel", lock |-> l]
Sleep == [op |-> "sleep", lock |-> None]

PluginCurrent ==
  << Acq("H"), Acq("F"), Acq("P"), Rel("P"), Sleep,
     Acq("P"), Rel("P"), Rel("F"), Rel("H") >>

PluginFixed ==
  << Acq("H"), Rel("H"), Acq("F"), Rel("F"), Acq("P"), Rel("P"), Sleep,
     Acq("F"), Acq("P"), Rel("P"), Rel("F") >>

CoordCurrent == << Acq("F"), Acq("H"), Rel("H"), Rel("F") >>

CoordFixed == << Acq("H"), Rel("H"), Acq("F"), Rel("F") >>

Backend == << Acq("F"), Acq("P"), Rel("P"), Rel("F") >>

Prog(t) ==
  CASE t = "plugin" -> IF Fixed THEN PluginFixed ELSE PluginCurrent
    [] t = "coord" -> IF Fixed THEN CoordFixed ELSE CoordCurrent
    [] t = "backend" -> Backend

VARIABLES pc, runs, holder

vars == <<pc, runs, holder>>

Finished(t) == pc[t] > Len(Prog(t))
Op(t) == Prog(t)[pc[t]]

Wants(t) ==
  IF ~Finished(t) /\ Op(t).op = "acq" THEN Op(t).lock ELSE None

Sleeping(t) == ~Finished(t) /\ Op(t).op = "sleep"

TypeOK ==
  /\ pc \in [Threads -> 1..12]
  /\ runs \in [Threads -> 0..Runs]
  /\ holder \in [Locks -> Threads \cup {None}]

Init ==
  /\ pc = [t \in Threads |-> 1]
  /\ runs = [t \in Threads |-> 0]
  /\ holder = [l \in Locks |-> None]

Advance(t) == pc' = [pc EXCEPT ![t] = @ + 1]

Step(t) ==
  /\ ~Finished(t)
  /\ LET o == Op(t) IN
       CASE o.op = "acq" ->
              /\ holder[o.lock] = None
              /\ holder' = [holder EXCEPT ![o.lock] = t]
              /\ Advance(t)
         [] o.op = "rel" ->
              /\ holder[o.lock] = t
              /\ holder' = [holder EXCEPT ![o.lock] = None]
              /\ Advance(t)
         [] o.op = "sleep" ->
              /\ UNCHANGED holder
              /\ Advance(t)
  /\ UNCHANGED runs

Repeat(t) ==
  /\ Finished(t)
  /\ runs[t] + 1 < Runs
  /\ runs' = [runs EXCEPT ![t] = @ + 1]
  /\ pc' = [pc EXCEPT ![t] = 1]
  /\ UNCHANGED holder

Done ==
  /\ \A t \in Threads : Finished(t) /\ runs[t] + 1 >= Runs
  /\ UNCHANGED vars

Next == \/ \E t \in Threads : Step(t) \/ Repeat(t)
        \/ Done

Spec == Init /\ [][Next]_vars /\ \A t \in Threads : WF_vars(Step(t) \/ Repeat(t))

NoLockCycle ==
  ~\E a, b \in Threads :
     /\ a # b
     /\ Wants(a) # None
     /\ Wants(b) # None
     /\ holder[Wants(a)] = b
     /\ holder[Wants(b)] = a

BackendNeverWaitsOnSleep ==
  ~(Wants("backend") = "F" /\ holder["F"] = "plugin" /\ Sleeping("plugin"))

AllFinish == <>(\A t \in Threads : Finished(t) /\ runs[t] + 1 >= Runs)
=============================================================================
