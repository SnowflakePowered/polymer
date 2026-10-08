---------------------------- MODULE VolumeDeletion ----------------------------
EXTENDS TLC
CONSTANTS Volumes, Target, EarlyClear, BlockRecovery
VARIABLES roots, components, transactions, index, checkout, intent, started, done
vars == <<roots, components, transactions, index, checkout, intent, started, done>>
Init ==
    /\ roots = Volumes
    /\ components = Volumes
    /\ transactions = {Target}
    /\ index = Volumes
    /\ checkout = Volumes
    /\ intent = FALSE
    /\ started = FALSE
    /\ done = FALSE
Begin ==
    /\ ~started
    /\ intent' = TRUE
    /\ started' = TRUE
    /\ UNCHANGED <<roots, components, transactions, index, checkout, done>>
RemoveRoot ==
    /\ intent /\ Target \in roots
    /\ roots' = roots \ {Target}
    /\ UNCHANGED <<components, transactions, index, checkout, intent, started, done>>
RemoveComponents ==
    /\ intent /\ Target \in components
    /\ components' = components \ {Target}
    /\ UNCHANGED <<roots, transactions, index, checkout, intent, started, done>>
RemoveTransaction ==
    /\ intent /\ Target \in transactions
    /\ transactions' = transactions \ {Target}
    /\ UNCHANGED <<roots, components, index, checkout, intent, started, done>>
RefsGone == Target \notin roots \cup components \cup transactions
RemoveIndex ==
    /\ intent /\ RefsGone /\ Target \in index
    /\ index' = index \ {Target}
    /\ UNCHANGED <<roots, components, transactions, checkout, intent, started, done>>
RemoveCheckout ==
    /\ intent /\ RefsGone /\ Target \notin index /\ Target \in checkout
    /\ checkout' = checkout \ {Target}
    /\ UNCHANGED <<roots, components, transactions, index, intent, started, done>>
Clear ==
    /\ intent /\ RefsGone
    /\ EarlyClear \/ Target \notin index \cup checkout
    /\ intent' = FALSE
    /\ done' = TRUE
    /\ UNCHANGED <<roots, components, transactions, index, checkout, started>>
RecoverInstall ==
    /\ (~intent \/ ~BlockRecovery) /\ Target \in transactions
    /\ roots' = roots \cup {Target}
    /\ components' = components \cup {Target}
    /\ index' = index \cup {Target}
    /\ checkout' = checkout \cup {Target}
    /\ transactions' = transactions \ {Target}
    /\ UNCHANGED <<intent, started, done>>
Next == Begin \/ RemoveRoot \/ RemoveComponents \/ RemoveTransaction
        \/ RemoveIndex \/ RemoveCheckout \/ Clear \/ RecoverInstall
TypeOK ==
    /\ roots \subseteq Volumes /\ components \subseteq Volumes
    /\ transactions \subseteq Volumes /\ index \subseteq Volumes /\ checkout \subseteq Volumes
    /\ intent \in BOOLEAN /\ started \in BOOLEAN /\ done \in BOOLEAN
    /\ done => started /\ ~intent
RecoveryEnabled == started /\ ~done =>
    ENABLED (RemoveRoot \/ RemoveComponents \/ RemoveTransaction \/ RemoveIndex \/ RemoveCheckout \/ Clear)
NoResurrection == [][started => roots' \subseteq roots /\ components' \subseteq components]_vars
CompleteCleanup == done => Target \notin roots \cup components \cup transactions \cup index \cup checkout
PendingRecoverable == started /\ ~done => intent
OtherVolumesPreserved == Volumes \ {Target} \subseteq roots \cap components \cap index \cap checkout
EventuallyDeleted == <>done
Spec == Init /\ [][Next]_vars /\ WF_vars(Begin) /\ WF_vars(RemoveRoot)
        /\ WF_vars(RemoveComponents) /\ WF_vars(RemoveTransaction)
        /\ WF_vars(RemoveIndex) /\ WF_vars(RemoveCheckout) /\ WF_vars(Clear)
=============================================================================
