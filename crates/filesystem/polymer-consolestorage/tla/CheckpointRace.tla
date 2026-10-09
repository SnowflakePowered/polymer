---------------------------- MODULE CheckpointRace ----------------------------
EXTENDS Integers, Sequences, FiniteSets
CONSTANTS Saves, Files, Writers, Target, Style, Blocks, MaxSaves,
          Check, Scope, PluginHint, ExpectComplete
Paths == UNION {{Files[s][i] : i \in 1..Len(Files[s])} : s \in Saves}
Cells(s) == [i \in 1..(Len(Files[s]) * Blocks) |->
                [file |-> Files[s][((i - 1) \div Blocks) + 1], block |-> ((i - 1) % Blocks) + 1]]
Initial == [p \in Paths |-> [b \in 1..Blocks |-> 0]]
RECURSIVE SumOver(_, _)
SumOver(f, S) == IF S = {} THEN 0 ELSE LET x == CHOOSE x \in S : TRUE IN f[x] + SumOver(f, S \ {x})

VARIABLES files, guest, calls, handles, generation, seq, mtime, busy,
          quiescentFiles, quiescentSaves, pc, target, cursor, snapshot, buffer,
          committed, tornFile, partialSave, partialBounded
vars == <<files, guest, calls, handles, generation, seq, mtime, busy,
          quiescentFiles, quiescentSaves, pc, target, cursor, snapshot, buffer,
          committed, tornFile, partialSave, partialBounded>>
guestVars == <<files, guest, calls, handles, generation, seq, mtime, busy,
               quiescentFiles, quiescentSaves>>
committerVars == <<pc, target, cursor, snapshot, buffer, committed, tornFile, partialSave, partialBounded>>

SaveOf(content, s) ==
    [save |-> s, files |-> [p \in {Files[s][i] : i \in 1..Len(Files[s])} |-> content[p]]]
InScope(s) == IF Scope = "save" THEN {s} ELSE Saves
Counters(s) ==
    [calls |-> IF Check \in {"calls", "handles"} THEN SumOver(calls, InScope(s)) ELSE 0,
     generation |-> IF Check \in {"calls", "handles"} THEN SumOver(generation, InScope(s)) ELSE 0,
     seq |-> IF Check = "seqlock" THEN SumOver(seq, InScope(s)) ELSE 0,
     mtime |-> IF Check = "mtime" THEN SumOver(mtime, InScope(s)) ELSE 0]

Init ==
    /\ files = Initial
    /\ guest = [w \in Writers |-> [phase |-> "idle", version |-> 0, saves |-> 0,
                                   file |-> 1, block |-> 1, inCall |-> FALSE]]
    /\ calls = [s \in Saves |-> 0] /\ generation = [s \in Saves |-> 0]
    /\ seq = [s \in Saves |-> 0] /\ mtime = [s \in Saves |-> 0]
    /\ handles = [s \in Saves |-> Cardinality({w \in Writers : Target[w] = s /\ Style[w] = "hold"})]
    /\ busy = [s \in Saves |-> FALSE]
    /\ quiescentFiles = {[path |-> p, content |-> Initial[p]] : p \in Paths}
    /\ quiescentSaves = {SaveOf(Initial, s) : s \in Saves}
    /\ pc = "idle" /\ target = (CHOOSE s \in Saves : TRUE) /\ cursor = 1
    /\ snapshot = Counters(CHOOSE s \in Saves : TRUE)
    /\ buffer = Initial
    /\ committed = [s \in Saves |-> SaveOf(Initial, s)]
    /\ tornFile = FALSE /\ partialSave = FALSE /\ partialBounded = FALSE

BeginSave(w) ==
    /\ guest[w].phase = "idle" /\ guest[w].saves < MaxSaves[w]
    /\ guest' = [guest EXCEPT ![w] = [phase |-> "saving", version |-> @.version + 1,
                                      saves |-> @.saves + 1, file |-> 1, block |-> 1,
                                      inCall |-> FALSE]]
    /\ busy' = IF PluginHint THEN [busy EXCEPT ![Target[w]] = TRUE] ELSE busy
    /\ IF Style[w] = "pass"
       THEN calls' = [calls EXCEPT ![Target[w]] = @ + 1] /\ seq' = [seq EXCEPT ![Target[w]] = @ + 1]
       ELSE UNCHANGED <<calls, seq>>
    /\ UNCHANGED <<files, handles, generation, mtime, quiescentFiles,
                   quiescentSaves, committerVars>>

StartCall(w) ==
    LET s == Target[w] IN
    /\ guest[w].phase = "saving" /\ ~guest[w].inCall /\ guest[w].file <= Len(Files[s])
    /\ guest' = [guest EXCEPT ![w].inCall = TRUE]
    /\ IF Style[w] = "pass"
       THEN UNCHANGED <<calls, seq>>
       ELSE calls' = [calls EXCEPT ![s] = @ + 1] /\ seq' = [seq EXCEPT ![s] = @ + 1]
    /\ handles' = IF Style[w] = "close" THEN [handles EXCEPT ![s] = @ + 1] ELSE handles
    /\ UNCHANGED <<files, generation, mtime, busy, quiescentFiles, quiescentSaves,
                   committerVars>>

WriteBlock(w) ==
    LET s == Target[w]
        p == Files[s][guest[w].file] IN
    /\ guest[w].inCall /\ guest[w].block <= Blocks
    /\ IF Style[w] \in {"installs", "pass"}
       THEN /\ files' = [files EXCEPT ![p] = [b \in 1..Blocks |-> guest[w].version]]
            /\ guest' = [guest EXCEPT ![w].block = Blocks + 1]
       ELSE /\ files' = [files EXCEPT ![p][guest[w].block] = guest[w].version]
            /\ guest' = [guest EXCEPT ![w].block = @ + 1]
    /\ UNCHANGED <<calls, handles, generation, seq, mtime, busy, quiescentFiles,
                   quiescentSaves, committerVars>>

EndCall(w) ==
    LET s == Target[w]
        p == Files[s][guest[w].file] IN
    /\ guest[w].inCall /\ guest[w].block > Blocks
    /\ guest' = [guest EXCEPT ![w].inCall = FALSE, ![w].block = 1, ![w].file = @ + 1]
    /\ IF Style[w] = "pass"
       THEN UNCHANGED <<calls, seq>>
       ELSE calls' = [calls EXCEPT ![s] = @ - 1] /\ seq' = [seq EXCEPT ![s] = @ + 1]
    /\ generation' = [generation EXCEPT ![s] = @ + 1]
    /\ mtime' = [mtime EXCEPT ![s] = guest[w].version]
    /\ handles' = IF Style[w] = "close" THEN [handles EXCEPT ![s] = @ - 1] ELSE handles
    /\ quiescentFiles' = quiescentFiles \cup {[path |-> p, content |-> files[p]]}
    /\ UNCHANGED <<files, busy, quiescentSaves, committerVars>>

EndSave(w) ==
    LET s == Target[w] IN
    /\ guest[w].phase = "saving" /\ ~guest[w].inCall /\ guest[w].file > Len(Files[s])
    /\ guest' = [guest EXCEPT ![w].phase = "idle"]
    /\ busy' = [busy EXCEPT ![s] = FALSE]
    /\ quiescentSaves' = quiescentSaves \cup {SaveOf(files, s)}
    /\ IF Style[w] = "pass"
       THEN /\ calls' = [calls EXCEPT ![s] = @ - 1] /\ seq' = [seq EXCEPT ![s] = @ + 1]
            /\ generation' = [generation EXCEPT ![s] = @ + 1]
       ELSE UNCHANGED <<calls, seq, generation>>
    /\ UNCHANGED <<files, handles, mtime, quiescentFiles, committerVars>>

Bounded(s) == PluginHint \/ s \in ExpectComplete
HandlesClosed(s) == Check = "handles" => SumOver(handles, InScope(s)) = 0
Gate(s) == ~busy[s] /\ HandlesClosed(s)

Unchanged(s) ==
    LET now == Counters(s) IN
    CASE Check = "none" -> TRUE
      [] Check = "mtime" -> now.mtime = snapshot.mtime
      [] Check = "seqlock" -> now.seq = snapshot.seq /\ now.seq % 2 = 0
      [] Check \in {"calls", "handles"} ->
           /\ now.calls = 0 /\ snapshot.calls = 0
           /\ now.generation = snapshot.generation

StartStaging(s) ==
    /\ pc = "idle" /\ Gate(s)
    /\ pc' = "read" /\ target' = s /\ cursor' = 1 /\ snapshot' = Counters(s)
    /\ UNCHANGED <<buffer, committed, tornFile, partialSave, partialBounded, guestVars>>

ReadBlock ==
    LET cell == Cells(target)[cursor] IN
    /\ pc = "read"
    /\ buffer' = [buffer EXCEPT ![cell.file][cell.block] = files[cell.file][cell.block]]
    /\ pc' = IF cursor = Len(Cells(target)) THEN "check" ELSE "read"
    /\ cursor' = IF cursor = Len(Cells(target)) THEN 1 ELSE cursor + 1
    /\ UNCHANGED <<target, snapshot, committed, tornFile, partialSave, partialBounded, guestVars>>

Commit ==
    /\ pc = "check"
    /\ IF HandlesClosed(target) /\ Unchanged(target)
       THEN /\ committed' = [committed EXCEPT ![target] = SaveOf(buffer, target)]
            /\ tornFile' = (tornFile \/ \E i \in 1..Len(Files[target]) :
                   [path |-> Files[target][i], content |-> buffer[Files[target][i]]] \notin quiescentFiles)
            /\ partialSave' = (partialSave \/ SaveOf(buffer, target) \notin quiescentSaves)
            /\ partialBounded' = (partialBounded \/ (Bounded(target) /\ SaveOf(buffer, target) \notin quiescentSaves))
       ELSE UNCHANGED <<committed, tornFile, partialSave, partialBounded>>
    /\ pc' = "idle"
    /\ UNCHANGED <<target, cursor, snapshot, buffer, guestVars>>

GuestStep == \E w \in Writers :
    BeginSave(w) \/ StartCall(w) \/ WriteBlock(w) \/ EndCall(w) \/ EndSave(w)
Next == GuestStep \/ (\E s \in Saves : StartStaging(s)) \/ ReadBlock \/ Commit \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
    /\ \A w \in Writers : WF_vars(StartCall(w)) /\ WF_vars(WriteBlock(w))
                          /\ WF_vars(EndCall(w)) /\ WF_vars(EndSave(w))
    /\ \A s \in Saves : SF_vars(StartStaging(s))
    /\ WF_vars(ReadBlock) /\ WF_vars(Commit)

Settled(s) == \A w \in Writers : Target[w] = s =>
    guest[w].phase = "idle" /\ guest[w].saves = MaxSaves[w]

TypeOK ==
    /\ pc \in {"idle", "read", "check"}
    /\ target \in Saves
    /\ \A s \in Saves : calls[s] \in 0..Cardinality(Writers)
FilesComplete == ~tornFile
SavesComplete == ~partialSave
BoundedSavesComplete == ~partialBounded
CheckpointsResume == \A s \in Saves : Settled(s) ~> (committed[s] = SaveOf(files, s))
=============================================================================
