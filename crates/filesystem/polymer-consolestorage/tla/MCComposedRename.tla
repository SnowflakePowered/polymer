------------------------- MODULE MCComposedRename -------------------------
EXTENDS ComposedRename, TLC

T == <<"t">>
K == <<"k">>
D == <<"d">>
DF == <<"d", "f">>
E == <<"e">>
EF == <<"e", "f">>

MCLayers == {"scratch", "keys", "saves"}
MCDurable == {"keys", "saves"}
MCPaths == {T, <<"~t">>, K, D, DF, <<"d", "~f">>, E, EF, <<"e", "~f">>}
MCOwner == [p \in MCPaths |->
              IF p \in {T, <<"~t">>} THEN "scratch"
              ELSE IF p = K THEN "keys"
              ELSE "saves"]
MCFilePaths == {T, K, DF, EF}
MCRenameOps == {<<T, DF>>, <<K, DF>>, <<D, E>>, <<DF, T>>}
MCInitNs == [l \in MCLayers |-> [p \in MCPaths |->
               IF l = "keys" /\ p = K THEN 1
               ELSE IF l = "saves" /\ p = D THEN DIR
               ELSE 0]]
MCHandles == {"h1", "h2"}
=============================================================================
