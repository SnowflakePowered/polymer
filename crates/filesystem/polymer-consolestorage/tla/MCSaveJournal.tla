---------------------------- MODULE MCSaveJournal ----------------------------
EXTENDS SaveJournal, TLC

MCProcs == {"p1", "p2"}
MCDepots == {"d1", "d2"}
MCDepot == [p \in MCProcs |-> "d1"]
MCCrossDepot == ("p1" :> "d1") @@ ("p2" :> "d2")
MCPaths == {"a", "b", "c"}
MCSnapshots == {0, 1, 2}
MCContents == (0 :> {}) @@ (1 :> {"a", "b"}) @@ (2 :> {"b", "c"})
MCTarget == ("p1" :> 2) @@ ("p2" :> 0)
=============================================================================
