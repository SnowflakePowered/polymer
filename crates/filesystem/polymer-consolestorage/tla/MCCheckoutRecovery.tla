------------------------- MODULE MCCheckoutRecovery -------------------------
EXTENDS CheckoutRecovery
MCSaves == {"A", "B"}
MCFiles == [s \in MCSaves |-> IF s = "A" THEN {"a1", "a2"} ELSE {"b1"}]
MCInitial == [p \in {"a1", "a2", "b1"} |-> IF p = "b1" THEN "absent" ELSE "v0"]
=============================================================================
