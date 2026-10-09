---------------------------- MODULE MCCheckpointRace ----------------------------
EXTENDS CheckpointRace
MCSaves == {"A", "B"}
MCFiles == [s \in MCSaves |-> IF s = "A" THEN <<"a1", "a2">> ELSE <<"b1">>]
MCWriters == {"game", "pdm"}
MCGameOnly == {"game"}
MCTarget == [w \in MCWriters |-> IF w = "game" THEN "A" ELSE "B"]
MCHold == [w \in MCWriters |-> "hold"]
MCClose == [w \in MCWriters |-> IF w = "game" THEN "close" ELSE "hold"]
MCInstalls == [w \in MCWriters |-> IF w = "game" THEN "installs" ELSE "hold"]
MCPass == [w \in MCWriters |-> IF w = "game" THEN "pass" ELSE "hold"]
MCMax == [w \in MCWriters |-> IF w = "game" THEN 2 ELSE 1]
=============================================================================
