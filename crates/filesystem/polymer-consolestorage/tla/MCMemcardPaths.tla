--------------------------- MODULE MCMemcardPaths ---------------------------
EXTENDS MemcardPaths
MCIds == {"a", "b", "c"}
MCPaths == {"p", "q"}
MCPathOf == [i \in MCIds |-> IF i = "c" THEN "q" ELSE "p"]
=============================================================================
