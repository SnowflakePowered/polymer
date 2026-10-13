# Library sources verification

Two models cover watched library folders and titles mounted in place from them.
They were written before the implementation, so the switches describe design
choices rather than existing code.

## Reconciling sources into the catalog

`LibrarySources` models one ROM file (versioned, possibly half-written), one
title directory whose marker file (`meta/meta.xml`, `PARAM.SFO`) decides whether
it is an in-place title, and an inner file (`USRDIR/EBOOT.BIN`) that is a
standalone unit only while the marker is absent. Two sources overlap: `games`
covers everything and `wiiu` covers the title directory, like `D:\Games` and
`D:\Games\wiiu`.

Every input becomes a reconcile request for a catalog key: watcher events after
debouncing, scan steps (the walk plus the catalog rows under the root), theme
imports, and source removal. A worker identifies the key on disk, then commits
in one catalog-writer transaction. Watch events can be dropped (overflow), the
drive can go offline and change while offline, and sources can be added and
removed at any point.

Properties: `NoFalseDelete` (an unreachable root never deletes a covered row),
`ManualKept` (drag-and-drop imports belong to no source and are never removed by
source management),
`NoOrphanOwner` (no row references a removed source), and `Converges` (once the
environment settles on a reachable drive, the catalog matches the disk exactly).

The design that passes:

- Ownership is derived, not stored. A row is source-managed iff an active
  source's root covers its path, evaluated inside the commit transaction. Rows
  imported by drag and drop carry a `manual` flag and belong to no source.
- Removing a source deletes, in the same transaction and without a disk read,
  every non-manual row under its root that no other active source covers.
- Reconciles are serialized per key; a request that arrives while the key is in
  flight reruns it after the commit.
- The watcher starts before the initial walk.
- Watcher overflow and drive reconnection request a full rescan. An unreachable
  root reads as an error, never as "absent".
- A marker change reconciles every key under its directory.

| Config | Switch | Result |
|---|---|---|
| `MCLibrarySources.cfg` | design above | Pass, including `Converges`: 32,860,616 states |
| `MCLibrarySourcesTwoWorkers.cfg` | two workers | Pass, including `Converges`: 7,129,063 states (one environment step, no overflow) |
| `MCLibrarySourcesOfflineAbsent.cfg` | unreachable reads as absent | **Violated** `NoFalseDelete`: a reconcile during an outage deletes rows for files that still exist |
| `MCLibrarySourcesSingleOwner.cfg` | `launchables.source_id` = requesting source | **Violated** `NoOrphanOwner`: an import queued before removal commits afterwards and recreates the row |
| `MCLibrarySourcesScanFirst.cfg` | watcher starts after the walk | **Violated** `Converges`: a file created behind the walk before the watcher starts is never imported |
| `MCLibrarySourcesSwallow.cfg` | deduped against the in-flight job (`JobStore` dedupe today) | **Violated** `Converges`: a change to a path while its import is in flight is lost (the request is merged into the running job) |
| `MCLibrarySourcesNoOverflowRescan.cfg` | overflow drops events silently | **Violated** `Converges`: dropped events are never recovered |
| `MCLibrarySourcesNoBoundary.cfg` | marker change reconciles only the directory | **Violated** `Converges`: `EBOOT.BIN` imported before `PARAM.SFO` arrives stays as a separate game |
| `MCLibrarySourcesUnserialized.cfg` | no per-key exclusion, two workers | **Violated** `Converges`: an older identify commits after a newer one (one environment step) |

Bounds: two sources, ROM versions up to 2, one theme import, and either one
worker with two environment steps and one overflow, or two workers with one
environment step and no overflow. Liveness assumes writers finish.

Found during modelling: removing a source while its drive is offline left rows
it had imported orphaned forever, because the removal's reconcile read the root
as unreachable and no source remained to rescan. Removal now deletes rows
directly in its transaction.

Implementation notes (`polymer-runtime/src/sources`): notify 8.2 on Windows
reports neither a `ReadDirectoryChangesW` overflow nor a dropped watch, so
`RescanOnOverflow` is realized by rebuilding every watch and rescanning every
30 minutes. Rescans stay cheap because a launchable records the size and
modification time it was imported from and unchanged units are skipped.

## Units, markers, moves, and mid-reconcile outages

`SourceUnits` models one title folder that can be renamed between two places
or deleted, a marker file that is absent, valid, or invalid (a PS3 patch, an
encrypted Vita dump), and an inner file that is its own unit only while the
folder is not a title. A reconcile stats, then probes (identify, locate), then
commits, so the folder can change or the drive can vanish between steps. It
was added after review found a reconcile loop the first model could not
express, because that model treated every marker as making a title.

| Config | Switch | Result |
|---|---|---|
| `MCSourceUnits.cfg` | a marker always reconciles its folder, also when deleted; expanding a folder never requests marker files; renames keep their destination; a probe of an unreachable drive is an error | Pass, `NoFalseDelete`, `Settles`, `Converges`: 1,652,307 states |
| `MCSourceUnitsMarkerLoop.cfg` | expansion also requests marker files | **Violated** `Settles`: an invalid marker and its folder request each other forever |
| `MCSourceUnitsMarkerKind.cfg` | a marker follows its folder only when the folder's catalog kind changed | **Violated** `Converges`: an inner file dropped while its folder was briefly a title is never re-imported |
| `MCSourceUnitsMarkerDelete.cfg` | a deleted marker is not followed | **Violated** `Converges`: the folder stays a title |
| `MCSourceUnitsRenameDropped.cfg` | the watcher drops a renamed folder's destination | **Violated** `Converges` |
| `MCSourceUnitsProbeAbsent.cfg` | an unreachable probe reads as "not a unit" | **Violated** `NoFalseDelete` |

Bounds: three environment steps. In the implementation, every forget re-checks
that the source root is reachable, and marker files are never units.

## Mounted titles and managed installs

`MountedTitles` (`crates/filesystem/polymer-consolestorage/tla/`) models a base
and an update that may exist externally (mounted in place), in the managed NAND,
or both. The NAND lease serializes installs, uninstalls, and sessions; catalog
commits follow storage operations asynchronously. Composition picks the managed
copy per install path.

| Config | Switch | Result |
|---|---|---|
| `MCMountedTitles.cfg` | Wii U layout; catalog hides the `mount://` launchable while an artifact exists | Pass, 15,810 states |
| `MCMountedTitlesPs3.cfg` | PS3 layout: updates share the base's directory | Pass, 17,113 states |
| `MCMountedTitlesPs3Updates.cfg` | `UpdatesInstallable` on PS3 | **Violated**: an update PKG for an externally mounted base is rejected (`accepts_version` needs a managed base) |
| `MCMountedTitlesComposeOnly.cfg` | no catalog shadowing | **Violated** `NoDuplicateLaunchable` |
| `MCMountedTitlesReconcileOnly.cfg` | only reconcile recomputes shadowing | **Violated** `NoDuplicateLaunchable` |

Shadowing must be recomputed in the same catalog transaction as both
`mounted_titles` commits and artifact launchable commits. A PS3 title mounted
in place cannot take managed updates; its external directory carries its own.
