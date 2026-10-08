-------------------------- MODULE MCTransportHandshake --------------------------
EXTENDS TransportHandshake, TLC

MCConns == {"c1", "c2"}
MCRank == ("c1" :> 1) @@ ("c2" :> 0)
=============================================================================
