-------------------------- MODULE NamespaceRename --------------------------
\* PreparedFilesystem's tree_files cache against a concurrent rename or remove.
\* Paths S and T are the cache keys a mutation touches; with Kind = "dir" they
\* stand for S/f and T/f, files inside the renamed directory. A cache entry pins
\* a host handle that follows its object across renames. Host handles share
\* delete, so a file rename succeeds while open, but a directory rename fails if
\* any descendant is open, and a non-POSIX delete stays pending until the last
\* handle closes.
EXTENDS Integers, FiniteSets
CONSTANTS Design, Kind, Op, Replace, MaxObjects

Paths == {"S", "T"}
PENDING == -1
Designs == {"after", "before", "both", "fenced"}
Evicts(phase) == \/ phase = "start" /\ Design \in {"before", "both", "fenced"}
                 \/ phase = "finish" /\ Design \in {"after", "both", "fenced"}

VARIABLES ns, cache, next, gen, fence, rpc, gpc, gop, gpath, gobj, ggen,
          held, staleCreate, staleResolve, spurious
vars == <<ns, cache, next, gen, fence, rpc, gpc, gop, gpath, gobj, ggen,
          held, staleCreate, staleResolve, spurious>>

Object(p) == ns[p] > 0
Handles == {cache[p] : p \in Paths} \cup held
           \cup (IF gpc = "looked" THEN {gobj} ELSE {})
RealHandles == held \cup (IF gpc = "looked" THEN {gobj} ELSE {})
Evicted == [p \in Paths |-> 0]

Init ==
    /\ ns = [p \in Paths |-> IF p = "S" THEN 1
                             ELSE IF Replace /\ Op = "rename" THEN 2 ELSE 0]
    /\ cache = [p \in Paths |-> 0]
    /\ next = IF Replace /\ Op = "rename" THEN 3 ELSE 2
    /\ gen = 0 /\ fence = FALSE /\ rpc = "idle"
    /\ gpc = "idle" /\ gop = "none" /\ gpath = "S" /\ gobj = 0 /\ ggen = 0
    /\ held = {}
    /\ staleCreate = FALSE /\ staleResolve = FALSE /\ spurious = FALSE

\* resolve_node: a fresh host lookup, then remember_tree_file.
Lookup(p) ==
    /\ gpc = "idle" /\ Object(p)
    /\ gpc' = "looked" /\ gop' = "open" /\ gpath' = p /\ gobj' = ns[p] /\ ggen' = gen
    /\ UNCHANGED <<ns, cache, next, gen, fence, rpc, held, staleCreate, staleResolve, spurious>>

\* create_file: the host create, then remember_tree_file.
Create(p) ==
    /\ gpc = "idle" /\ ns[p] = 0 /\ next <= MaxObjects
    /\ ns' = [ns EXCEPT ![p] = next] /\ next' = next + 1
    /\ gpc' = "looked" /\ gop' = "create" /\ gpath' = p /\ gobj' = next /\ ggen' = gen
    /\ UNCHANGED <<cache, gen, fence, rpc, held, staleCreate, staleResolve, spurious>>

\* or_insert_with keeps an existing entry over the node the lookup just found.
\* The fenced design neither inserts nor reuses while a mutation is in flight,
\* nor after one completed since the lookup began.
Remember ==
    /\ gpc = "looked"
    /\ LET bypass == Design = "fenced" /\ (fence \/ ggen # gen)
           result == IF ~bypass /\ cache[gpath] # 0 THEN cache[gpath] ELSE gobj
       IN /\ cache' = IF bypass \/ cache[gpath] # 0 THEN cache
                      ELSE [cache EXCEPT ![gpath] = gobj]
          /\ held' = held \cup {result}
          /\ staleCreate' = (staleCreate \/ (gop = "create" /\ result # gobj))
          /\ staleResolve' = (staleResolve \/ (result # gobj /\ result # ns[gpath]))
    /\ gpc' = "open"
    /\ UNCHANGED <<ns, next, gen, fence, rpc, gop, gpath, gobj, ggen, spurious>>

Close ==
    /\ gpc = "open" /\ held' = {} /\ gpc' = "idle" /\ gop' = "none"
    /\ UNCHANGED <<ns, cache, next, gen, fence, rpc, gpath, gobj, ggen,
                   staleCreate, staleResolve, spurious>>

Start ==
    /\ rpc = "idle"
    /\ cache' = IF Evicts("start") THEN Evicted ELSE cache
    /\ fence' = (Design = "fenced")
    /\ gen' = IF Design = "fenced" THEN gen + 1 ELSE gen
    /\ rpc' = "started"
    /\ UNCHANGED <<ns, next, gpc, gop, gpath, gobj, ggen, held, staleCreate, staleResolve, spurious>>

Blocked == Kind = "dir" /\ ns["S"] \in Handles

RenameHost ==
    /\ rpc = "started" /\ Op = "rename"
    /\ IF Blocked
       THEN /\ rpc' = "failed" /\ ns' = ns
            /\ spurious' = (spurious \/ ns["S"] \notin RealHandles)
       ELSE /\ rpc' = "mutated" /\ spurious' = spurious
            /\ ns' = [ns EXCEPT !["T"] = ns["S"], !["S"] = 0]
    /\ UNCHANGED <<cache, next, gen, fence, gpc, gop, gpath, gobj, ggen, held, staleCreate, staleResolve>>

RemoveHost ==
    /\ rpc = "started" /\ Op = "remove"
    /\ ns' = [ns EXCEPT !["S"] = IF ns["S"] \in Handles THEN PENDING ELSE 0]
    /\ rpc' = "mutated"
    /\ UNCHANGED <<cache, next, gen, fence, gpc, gop, gpath, gobj, ggen, held, staleCreate, staleResolve, spurious>>

\* The pending object is the one removed from S: object 1.
DeleteCompletes ==
    /\ ns["S"] = PENDING /\ 1 \notin Handles
    /\ ns' = [ns EXCEPT !["S"] = 0]
    /\ UNCHANGED <<cache, next, gen, fence, rpc, gpc, gop, gpath, gobj, ggen, held, staleCreate, staleResolve, spurious>>

Finish ==
    /\ rpc \in {"mutated", "failed"}
    /\ cache' = IF rpc = "mutated" /\ Evicts("finish") THEN Evicted ELSE cache
    /\ fence' = FALSE
    /\ gen' = IF Design = "fenced" THEN gen + 1 ELSE gen
    /\ rpc' = IF rpc = "mutated" THEN "done" ELSE "aborted"
    /\ UNCHANGED <<ns, next, gpc, gop, gpath, gobj, ggen, held, staleCreate, staleResolve, spurious>>

Guest == \/ \E p \in Paths : Lookup(p) \/ Create(p)
         \/ Remember \/ Close
Mutator == Start \/ RenameHost \/ RemoveHost \/ Finish

Next == Guest \/ Mutator \/ DeleteCompletes

Spec == Init /\ [][Next]_vars /\ WF_vars(Mutator) /\ WF_vars(Remember)
        /\ WF_vars(Close) /\ WF_vars(DeleteCompletes)

TypeOK ==
    /\ Design \in Designs /\ Kind \in {"file", "dir"} /\ Op \in {"rename", "remove"}
    /\ ns \in [Paths -> {PENDING} \cup 0..MaxObjects]
    /\ cache \in [Paths -> 0..MaxObjects]
    /\ rpc \in {"idle", "started", "mutated", "failed", "done", "aborted"}
    /\ gpc \in {"idle", "looked", "open"}
    /\ held \subseteq 1..MaxObjects

\* create_file hands back the file it just created.
CreateReturnsFresh == ~staleCreate
\* A resolve returns what its lookup found or what the path holds now.
ResolveSeesNamespace == ~staleResolve
\* Only handles a guest really holds can block a directory rename.
RenameBlockedOnlyByRealHandles == ~spurious
\* Outside an in-flight lookup or mutation, cached entries match the namespace.
CacheCoherentAtRest ==
    (rpc \in {"idle", "done", "aborted"} /\ gpc # "looked")
        => \A p \in Paths : cache[p] \in {0, ns[p]}

MutationCompletes == <>(rpc \in {"done", "aborted"})
PendingDeleteCompletes == (ns["S"] = PENDING) ~> (ns["S"] # PENDING)
=============================================================================
