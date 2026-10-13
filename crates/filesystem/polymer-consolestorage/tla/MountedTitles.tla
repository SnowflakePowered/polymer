---------------------------- MODULE MountedTitles ----------------------------
(*
  One title on one platform, present externally (a library source mounts it
  in place) and/or installed into the managed NAND.

  Components: "base" and "update". With SamePathUpdates (PS3), an update
  installs into the base's directory and is only accepted over an installed
  base version (Ps3Composition::apply / accepts_version). Otherwise (Wii U,
  Vita) the update has its own install path.

  Uninstalling a PS3 base also removes its updates: the component registry
  refuses with AffectedDepots, and the caller retries with the affected set.

  The NAND lease (ExclusiveResourceKey::Resource) serializes installs,
  uninstalls, and sessions. The catalog writer is one actor; each catalog
  commit is atomic, but it runs after the storage operation that caused it.

  Shadow:
    "compose"  a mount:// launchable is listed whenever its row exists; the
               managed copy only wins when composing a launch
    "catalog"  the catalog hides the mount:// launchable while an artifact
               launchable exists at the same install path, recomputed by
               reconcile commits and by artifact commits
    "reconcile" like "catalog", but only reconcile commits recompute it
*)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS SamePathUpdates, Shadow, MaxEnv, MaxOps

Parts == {"base", "update"}
Views == {"managed", "external", "none"}

VARIABLES ext, managed, lease, opPc, opPart, rows, recPc, recObs,
          artL, mountL, pick, session, view, env, ops, rejected, failed

vars == <<ext, managed, lease, opPc, opPart, rows, recPc, recObs,
          artL, mountL, pick, session, view, env, ops, rejected, failed>>

TypeOK ==
  /\ ext \subseteq Parts /\ managed \subseteq Parts
  /\ lease \in {"free", "op", "session"}
  /\ opPc \in {"idle", "catalog"} /\ opPart \in Parts
  /\ rows \subseteq Parts
  /\ recPc \in {"idle", "commit"} /\ recObs \subseteq Parts
  /\ artL \in BOOLEAN /\ mountL \in BOOLEAN
  /\ pick \in {"none", "artifact", "mount"}
  /\ session \in BOOLEAN
  /\ view \in [Parts -> Views]
  /\ rejected \in BOOLEAN /\ failed \in BOOLEAN

Init ==
  /\ ext \in SUBSET Parts /\ managed = {}
  /\ lease = "free" /\ opPc = "idle" /\ opPart = "base"
  /\ rows = {} /\ recPc = "idle" /\ recObs = {}
  /\ artL = FALSE /\ mountL = FALSE
  /\ pick = "none" /\ session = FALSE
  /\ view = [p \in Parts |-> "none"]
  /\ env = 0 /\ ops = 0 /\ rejected = FALSE /\ failed = FALSE

MountListed(r, a) ==
  "base" \in r /\ (Shadow = "compose" \/ ~a)

\* Install path occupied by a part; PS3 updates share the base's directory.
PathOf(p) == IF SamePathUpdates /\ p = "update" THEN "base" ELSE p

ManagedAt(path) == \E q \in managed : PathOf(q) = path

-----------------------------------------------------------------------------
\* The user's external folder (also written by their own emulator install).

ExtToggle(p) ==
  /\ env < MaxEnv /\ env' = env + 1
  /\ ext' = IF p \in ext THEN ext \ {p} ELSE ext \cup {p}
  /\ UNCHANGED <<managed, lease, opPc, opPart, rows, recPc, recObs, artL, mountL,
                 pick, session, view, ops, rejected, failed>>

\* Library reconcile of the source: identify, then commit mounted_titles rows.
RecRead ==
  /\ recPc = "idle" /\ ext # rows
  /\ recPc' = "commit" /\ recObs' = ext
  /\ UNCHANGED <<ext, managed, lease, opPc, opPart, rows, artL, mountL, pick, session,
                 view, env, ops, rejected, failed>>

RecCommit ==
  /\ recPc = "commit"
  /\ recPc' = "idle" /\ rows' = recObs
  /\ mountL' = MountListed(recObs, artL)
  /\ UNCHANGED <<ext, managed, lease, opPc, opPart, recObs, artL, pick, session, view,
                 env, ops, rejected, failed>>

-----------------------------------------------------------------------------
\* Manual installs and uninstalls (storage job, then catalog commit).

Install(p) ==
  /\ ops < MaxOps /\ lease = "free" /\ opPc = "idle" /\ p \notin managed
  /\ ops' = ops + 1
  /\ IF SamePathUpdates /\ p = "update" /\ "base" \notin managed
       THEN /\ rejected' = (rejected \/ "base" \in ext)
            /\ UNCHANGED <<managed, opPc, opPart>>
       ELSE /\ managed' = managed \cup {p}
            /\ opPc' = "catalog" /\ opPart' = p
            /\ UNCHANGED rejected
  /\ UNCHANGED <<ext, lease, rows, recPc, recObs, artL, mountL, pick, session, view, env, failed>>

Uninstall(p) ==
  /\ ops < MaxOps /\ lease = "free" /\ opPc = "idle" /\ p \in managed
  /\ ops' = ops + 1
  /\ managed' = managed \ (IF SamePathUpdates /\ p = "base" THEN Parts ELSE {p})
  /\ opPc' = "catalog" /\ opPart' = p
  /\ UNCHANGED <<ext, lease, rows, recPc, recObs, artL, mountL, pick, session, view, env,
                 rejected, failed>>

OpCatalog ==
  /\ opPc = "catalog"
  /\ opPc' = "idle"
  /\ artL' = ("base" \in managed)
  /\ mountL' = IF Shadow = "catalog" THEN MountListed(rows, "base" \in managed) ELSE mountL
  /\ UNCHANGED <<ext, managed, lease, opPart, rows, recPc, recObs, pick, session, view, env,
                 ops, rejected, failed>>

-----------------------------------------------------------------------------
\* The theme lists launchables; the user launches one later.

Pick(k) ==
  /\ pick = "none" /\ ~session
  /\ (k = "artifact" /\ artL) \/ (k = "mount" /\ mountL)
  /\ pick' = k
  /\ UNCHANGED <<ext, managed, lease, opPc, opPart, rows, recPc, recObs, artL, mountL,
                 session, view, env, ops, rejected, failed>>

\* Composition under the lease: the managed install wins per install path.
Compose(p) ==
  IF ManagedAt(PathOf(p))
    THEN IF p \in managed THEN "managed" ELSE "none"
  ELSE IF p \in rows /\ p \in ext THEN "external"
  ELSE "none"

Launch ==
  /\ pick # "none" /\ lease = "free" /\ opPc = "idle"
  /\ LET v == [p \in Parts |-> Compose(p)] IN
       /\ view' = v
       /\ IF v["base"] = "none"
            THEN /\ failed' = (failed \/ "base" \in managed \/ ("base" \in ext /\ "base" \in rows))
                 /\ pick' = "none"
                 /\ UNCHANGED <<lease, session>>
            ELSE /\ lease' = "session" /\ session' = TRUE
                 /\ UNCHANGED <<pick, failed>>
  /\ UNCHANGED <<ext, managed, opPc, opPart, rows, recPc, recObs, artL, mountL, env, ops, rejected>>

Quit ==
  /\ session
  /\ session' = FALSE /\ lease' = "free" /\ pick' = "none"
  /\ view' = [p \in Parts |-> "none"]
  /\ UNCHANGED <<ext, managed, opPc, opPart, rows, recPc, recObs, artL, mountL, env, ops,
                 rejected, failed>>

-----------------------------------------------------------------------------

Next ==
  \/ \E p \in Parts : ExtToggle(p) \/ Install(p) \/ Uninstall(p)
  \/ RecRead \/ RecCommit \/ OpCatalog
  \/ \E k \in {"artifact", "mount"} : Pick(k)
  \/ Launch \/ Quit

Spec == Init /\ [][Next]_vars
        /\ WF_vars(RecRead) /\ WF_vars(RecCommit) /\ WF_vars(OpCatalog) /\ WF_vars(Quit)

-----------------------------------------------------------------------------

\* The library never lists the same title twice.
NoDuplicateLaunchable == ~(artL /\ mountL)

\* A running session reads the managed copy whenever one is installed.
ManagedWins ==
  session => \A p \in Parts : (ManagedAt(PathOf(p)) => view[p] # "external")

\* A session never mixes a managed update with an external base (or vice versa) at one path.
NoMixedPath ==
  session /\ SamePathUpdates => ~(view["base"] = "external" /\ view["update"] = "managed")

\* Launch fails only when no copy of the base exists.
LaunchFailsOnlyWhenAbsent == ~failed

\* An update can be installed over an externally mounted base.
UpdatesInstallable == ~rejected

\* The mounted launchable is listed exactly when the external base is the only copy.
MountListedEventually ==
  <>[](mountL <=> ("base" \in ext /\ "base" \notin managed))

=============================================================================
