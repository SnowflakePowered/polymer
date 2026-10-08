---------------------- MODULE MCComposedRenameCorruption ----------------------
EXTENDS MCComposedRename
CONSTANTS MaxCorruptions, CheckPayload
VARIABLES payloadValid, corruptions, corruptPublished
cvars == <<vars, payloadValid, corruptions, corruptPublished>>

CorruptionInit == Init /\ payloadValid = TRUE /\ corruptions = 0
                     /\ corruptPublished = FALSE

Ordinary ==
    /\ (\E h \in Handles : WriteAt(h) \/ (\E p \in OpenPaths : Open(h, p)))
       \/ (\E p \in CreatePaths \cup ReplacePaths : Create(p))
       \/ (\E op \in RenameOps : Rename(op[1], op[2]))
       \/ CopyToStaged \/ CommitDecision \/ RemoveFrom \/ MoveBinding \/ Clear
       \/ Abort \/ Restore \/ RemoveFailure \/ Fail \/ Crash \/ Finished
    /\ payloadValid' = IF staged' = 0 THEN TRUE ELSE payloadValid
    /\ UNCHANGED <<corruptions, corruptPublished>>

CheckedPublish ==
    /\ ~CheckPayload \/ payloadValid
    /\ PublishStaged
    /\ corruptPublished' = (corruptPublished \/ ~payloadValid)
    /\ UNCHANGED <<payloadValid, corruptions>>

CorruptPayload ==
    /\ Journaled /\ txn # NoOp /\ staged > 0
    /\ rpc \in {"decide", "publish"}
    /\ payloadValid /\ corruptions < MaxCorruptions
    /\ payloadValid' = FALSE /\ corruptions' = corruptions + 1
    /\ UNCHANGED <<vars, corruptPublished>>

RepairPayload ==
    /\ ~payloadValid
    /\ payloadValid' = TRUE
    /\ UNCHANGED <<vars, corruptions, corruptPublished>>

CorruptionProgress ==
    (Ordinary /\ Progress) \/ CheckedPublish
CorruptionNext == Ordinary \/ CheckedPublish \/ CorruptPayload \/ RepairPayload
CorruptionSpec == CorruptionInit /\ [][CorruptionNext]_cvars
                  /\ WF_cvars(CorruptionProgress)
RepairedSpec == CorruptionSpec /\ WF_cvars(RepairPayload)

CorruptionTypeOK == TypeOK /\ payloadValid \in BOOLEAN
    /\ corruptions \in 0..MaxCorruptions /\ corruptPublished \in BOOLEAN
NoCorruptPublication == ~corruptPublished
CorruptionBlocked == rpc = "publish" /\ ~payloadValid =>
    txn # NoOp /\ committed /\ ~Available /\ ~ENABLED CheckedPublish
RecoveryOrBlocked == ~Idle =>
    (ENABLED CorruptionProgress) \/ (rpc = "publish" /\ ~payloadValid)
RecoveryAfterRepair == (<>[]payloadValid) => RenameSettles
=============================================================================
