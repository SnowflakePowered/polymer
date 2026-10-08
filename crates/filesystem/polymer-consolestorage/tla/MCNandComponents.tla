-------------------------- MODULE MCNandComponents --------------------------
EXTENDS NandComponents

MCComponents == {"main", "addon"}
MCRevisions == {"a1", "a2", "a3", "b1", "b2"}
MCPaths == {"x", "y", "sfo"}
MCIds == {"base", "update", "dlc"}
MCOwner == [r \in MCRevisions |-> IF r \in {"a1", "a2", "a3"} THEN "main" ELSE "addon"]
MCArtifact == [r \in MCRevisions |-> IF MCOwner[r] = "addon" THEN "dlc" ELSE IF r = "a1" \/ Mode # "ordered" THEN "base" ELSE "update"]
MCFiles ==
    [r \in MCRevisions |-> [p \in MCPaths |->
      CASE Mode = "ordered" ->
        CASE r = "a1" -> IF p = "y" THEN 0 ELSE 1
          [] r = "a2" -> IF p = "x" THEN 0 ELSE 2
          [] r = "a3" -> IF p = "sfo" THEN 3 ELSE IF p = "x" THEN 1 ELSE 0
          [] r = "b1" -> IF p = "x" THEN 2 ELSE 0
          [] OTHER -> IF p = "y" THEN 0 ELSE 1
        [] Mode = "shared" ->
          IF p = "sfo" THEN 0 ELSE
          CASE r = "a1" -> IF p = "x" THEN 1 ELSE 0
            [] r = "a2" -> IF p = "x" THEN 2 ELSE 0
            [] r = "a3" -> IF p = "x" THEN 1 ELSE 0
            [] r = "b1" -> IF p = "x" THEN 1 ELSE 0
            [] OTHER -> IF p = "y" THEN 2 ELSE 0
        [] OTHER ->
          IF MCOwner[r] = "main" /\ p = "x" THEN IF r = "a1" THEN 1 ELSE 2
          ELSE IF MCOwner[r] = "addon" /\ p = "y" THEN IF r = "b1" THEN 1 ELSE 2 ELSE 0]]
MCOverwrites == [r \in MCRevisions |-> IF r = "b2" THEN {} ELSE MCPaths]
MCTarget == [r \in MCRevisions |-> IF r = "b2" THEN 2 ELSE 0]
MCIsUpdate == [r \in MCRevisions |-> r \in {"a2", "a3"}]
MCDepends == [r \in MCRevisions |-> {}]
MCDependencies == [r \in MCRevisions |-> IF MCOwner[r] = "addon" THEN {"base"} ELSE {}]
=============================================================================
