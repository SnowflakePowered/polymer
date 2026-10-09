----------------------- MODULE ScopedDependencies -----------------------
EXTENDS Integers, FiniteSets
CONSTANTS Roots, Clients
VARIABLES owner, attached, pending, phase, held, registrations, cleaned, aliasAttempts
vars == <<owner, attached, pending, phase, held, registrations, cleaned, aliasAttempts>>
Init ==
    /\ owner = "none" /\ attached = [r \in Roots |-> {}]
    /\ pending = Clients /\ phase = [r \in Roots |-> "building"]
    /\ held = FALSE /\ registrations = 0 /\ cleaned = FALSE /\ aliasAttempts = {}
Attach(c, r) ==
    /\ c \in pending /\ phase[r] = "building"
    /\ pending' = pending \ {c}
    /\ IF owner = "none" \/ owner = r
       THEN /\ owner' = r /\ held' = TRUE
            /\ attached' = [attached EXCEPT ![r] = @ \cup {c}]
            /\ registrations' = IF owner = "none" THEN registrations + 1 ELSE registrations
       ELSE UNCHANGED <<owner, held, attached, registrations>>
    /\ UNCHANGED <<phase, cleaned, aliasAttempts>>
AliasAttempt(c) ==
    /\ c \notin aliasAttempts /\ aliasAttempts' = aliasAttempts \cup {c}
    /\ UNCHANGED <<owner, attached, pending, phase, held, registrations, cleaned>>
Begin(r) ==
    /\ pending = {} /\ phase[r] = "building"
    /\ phase' = [phase EXCEPT ![r] = "cleanup"]
    /\ UNCHANGED <<owner, attached, pending, held, registrations, cleaned, aliasAttempts>>
Cleanup(r) ==
    /\ phase[r] = "cleanup" /\ phase' = [phase EXCEPT ![r] = "clean"]
    /\ cleaned' = (cleaned \/ owner = r)
    /\ attached' = [attached EXCEPT ![r] = {}]
    /\ UNCHANGED <<owner, pending, held, registrations, aliasAttempts>>
Release(r) ==
    /\ phase[r] = "clean" /\ phase' = [phase EXCEPT ![r] = "done"]
    /\ held' = IF owner = r THEN FALSE ELSE held
    /\ UNCHANGED <<owner, attached, pending, registrations, cleaned, aliasAttempts>>
Step(r) == Begin(r) \/ Cleanup(r) \/ Release(r)
Next == (\E c \in Clients, r \in Roots : Attach(c,r))
    \/ (\E c \in Clients : AliasAttempt(c)) \/ (\E r \in Roots : Step(r)) \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
    /\ (\A c \in Clients : WF_vars(\E r \in Roots : Attach(c,r)))
    /\ (\A r \in Roots : WF_vars(Step(r)))
TypeOK == /\ owner \in Roots \cup {"none"} /\ pending \subseteq Clients
    /\ attached \in [Roots -> SUBSET Clients]
    /\ phase \in [Roots -> {"building", "cleanup", "clean", "done"}]
    /\ held \in BOOLEAN /\ cleaned \in BOOLEAN /\ registrations \in 0..1
    /\ aliasAttempts \subseteq Clients
SingleAuthority == \A r \in Roots : attached[r] # {} => owner = r
DependencyProtected == (\E r \in Roots : attached[r] # {}) => held
RegisteredOnce == owner # "none" => registrations = 1
ReleaseAfterCleanup == owner # "none" /\ ~held => cleaned
AllDone == <>(\A r \in Roots : phase[r] = "done")
=============================================================================
