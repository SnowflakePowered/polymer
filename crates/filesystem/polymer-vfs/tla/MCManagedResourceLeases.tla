---------------------- MODULE MCManagedResourceLeases ----------------------
EXTENDS ManagedResourceLeases
MCClients == {"L1", "L2"}
MCJobs == {"J1"}
MCKeys == {"card", "nand"}
MCRequired == [c \in MCClients |-> IF c = "L1" THEN <<"card", "nand">> ELSE <<"card">>]
=============================================================================
