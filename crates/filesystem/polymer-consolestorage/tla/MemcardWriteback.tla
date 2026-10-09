-------------------------- MODULE MemcardWriteback --------------------------
EXTENDS Integers, Sequences, FiniteSets
CONSTANTS Saves, Versions, Policy, Order, QuietReads, MaxOperations, MaxCrashes, MaxSessions
Fields == {"structure", "first", "second"}
Valid(save) == save.structure \in {"ok", "none"}
Empty == [structure |-> "none", first |-> 0, second |-> 0]
Decode(save) == IF save.structure = "none" THEN Empty ELSE save

VARIABLES phase, emulator, image, stored, pending, burst, guest, real,
          operations, crashes, sessions
vars == <<phase, emulator, image, stored, pending, burst, guest, real,
          operations, crashes, sessions>>
emulatorVars == <<image, pending, burst>>
counters == <<operations, crashes, sessions>>

Idle == [kind |-> "idle", save |-> CHOOSE s \in Saves : TRUE, version |-> 0, step |-> 0]
Steps(kind) == CASE kind = "overwrite" -> <<"first", "second">>
                 [] kind = "create" -> <<"open", "first", "second", "close">>
                 [] kind = "delete" -> <<"open", "clear">>
Publishing == Order = "burst" /\ image # burst
Running == phase \in {"running", "quitting"}

Init ==
    /\ phase = "running"
    /\ stored = [s \in Saves |-> Empty]
    /\ emulator = stored /\ image = stored
    /\ pending = <<>> /\ burst = stored
    /\ guest = Idle
    /\ real = [s \in Saves |-> {Empty}]
    /\ operations = 0 /\ crashes = 0 /\ sessions = 1

Apply(save, step, version) ==
    CASE step = "first" -> [save EXCEPT !.first = version]
      [] step = "second" -> [save EXCEPT !.second = version]
      [] step = "open" -> [save EXCEPT !.structure = "torn"]
      [] step = "close" -> [save EXCEPT !.structure = "ok"]
      [] step = "clear" -> Empty

Begin(kind, s, v) ==
    /\ phase = "running" /\ guest.kind = "idle" /\ operations < MaxOperations
    /\ kind \in {"overwrite", "delete"} => emulator[s].structure = "ok"
    /\ kind = "create" => emulator[s].structure = "none"
    /\ guest' = [kind |-> kind, save |-> s, version |-> v, step |-> 1]
    /\ operations' = operations + 1
    /\ UNCHANGED <<phase, emulator, stored, emulatorVars, real, crashes, sessions>>

GuestWrite ==
    /\ phase = "running" /\ guest.kind # "idle"
    /\ LET s == guest.save
           next == Apply(emulator[s], Steps(guest.kind)[guest.step], guest.version) IN
        /\ emulator' = [emulator EXCEPT ![s] = next]
        /\ real' = [real EXCEPT ![s] = @ \cup {next}]
        /\ pending' = IF Order = "inorder" THEN Append(pending, [save |-> s, value |-> next])
                      ELSE pending
        /\ guest' = IF guest.step = Len(Steps(guest.kind)) THEN Idle
                    ELSE [guest EXCEPT !.step = @ + 1]
    /\ UNCHANGED <<phase, image, stored, burst, counters>>

WriteThrough ==
    /\ Order = "inorder" /\ Running /\ pending # <<>>
    /\ image' = [image EXCEPT ![Head(pending).save] = Head(pending).value]
    /\ pending' = Tail(pending)
    /\ UNCHANGED <<phase, emulator, stored, burst, guest, real, counters>>

ReplaceAll ==
    /\ Order = "atomic" /\ Running /\ image # emulator
    /\ image' = emulator
    /\ UNCHANGED <<phase, emulator, stored, pending, burst, guest, real, counters>>

StartBurst ==
    /\ Order = "burst" /\ Running /\ ~Publishing /\ image # emulator
    /\ burst' = emulator
    /\ UNCHANGED <<phase, emulator, image, stored, pending, guest, real, counters>>

BurstWrite(s, field) ==
    /\ Running /\ Publishing
    /\ image[s][field] # burst[s][field]
    /\ image' = [image EXCEPT ![s][field] = burst[s][field]]
    /\ UNCHANGED <<phase, emulator, stored, pending, burst, guest, real, counters>>

Crash ==
    /\ phase = "running" /\ crashes < MaxCrashes
    /\ phase' = "ended" /\ guest' = Idle /\ pending' = <<>> /\ burst' = image
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<emulator, image, stored, real, operations, sessions>>

Quit ==
    /\ phase = "running"
    /\ phase' = "quitting" /\ guest' = Idle
    /\ UNCHANGED <<emulator, image, stored, pending, burst, real, counters>>

Closed ==
    /\ phase = "quitting" /\ image = emulator
    /\ phase' = "ended" /\ pending' = <<>>
    /\ UNCHANGED <<emulator, image, stored, burst, guest, real, counters>>

Keep(s) == Policy = "current" \/ Valid(image[s])

Pass ==
    /\ QuietReads => ~(Running /\ Publishing)
    /\ stored' = [s \in Saves |-> IF Keep(s) THEN Decode(image[s]) ELSE stored[s]]
    /\ UNCHANGED <<phase, emulator, image, pending, burst, guest, real, counters>>

Mount ==
    /\ phase = "ended" /\ sessions < MaxSessions
    /\ phase' = "running" /\ sessions' = sessions + 1
    /\ emulator' = stored /\ image' = stored
    /\ pending' = <<>> /\ burst' = stored /\ guest' = Idle
    /\ UNCHANGED <<stored, real, operations, crashes>>

Next ==
    \/ \E kind \in {"overwrite", "create", "delete"}, s \in Saves, v \in Versions : Begin(kind, s, v)
    \/ GuestWrite \/ WriteThrough \/ ReplaceAll \/ StartBurst
    \/ (\E s \in Saves, field \in Fields : BurstWrite(s, field))
    \/ Crash \/ Quit \/ Closed \/ Pass \/ Mount
    \/ UNCHANGED vars

Spec == Init /\ [][Next]_vars
    /\ WF_vars(Pass) /\ WF_vars(Closed) /\ WF_vars(WriteThrough) /\ WF_vars(ReplaceAll)
    /\ WF_vars(StartBurst) /\ \A s \in Saves, field \in Fields : WF_vars(BurstWrite(s, field))

TypeOK ==
    /\ phase \in {"running", "quitting", "ended"}
    /\ \A s \in Saves : image[s].structure \in {"ok", "none", "torn"}
GuestViewConsistent == \A s \in Saves : Valid(stored[s])
SessionEndConsistent == phase = "ended" => \A s \in Saves : Valid(stored[s])
StoredIsRealState == \A s \in Saves : stored[s] \in real[s]
EventuallyConsistent == \A s \in Saves :
    (phase = "ended" /\ Valid(image[s])) ~> (stored[s] = Decode(image[s]) \/ phase # "ended")
=============================================================================
