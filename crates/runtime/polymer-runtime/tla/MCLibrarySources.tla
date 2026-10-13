--------------------------- MODULE MCLibrarySources ---------------------------
EXTENDS LibrarySources, TLC

MCSources == {"games", "wiiu"}
MCCovers == [s \in MCSources |-> IF s = "games" THEN {"rom", "dir", "inner"} ELSE {"dir", "inner"}]
MCOneWorker == {"w1"}
MCTwoWorkers == {"w1", "w2"}
=============================================================================
