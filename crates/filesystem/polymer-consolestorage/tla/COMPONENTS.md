# NAND component verification

These models cover the current component-history implementation for Wii, Wii U,
3DS/New 3DS, Vita, and PS3. Superseded global-history models have been removed.

## Composition

`NandComponents` models selected per-component ancestry, monotonic installation
sequence numbers, snapshot replacement, ordered package operations, shared file
ownership, rollback, and removal dependency closure. `histories` corresponds to
`ComponentHistory::revisions`; `Selected` corresponds to `contributions` and the
platform's `includes_ancestors` policy. `Replay` is the composer, `Retained` is
`retained_revision`, and `Removing` is `affected`'s fixed point. Wii UID allocation
is retained separately from active histories.

Four configurations exercise disjoint snapshots, Wii-style shared snapshots,
explicit removal dependencies, and PS3-style ordered writes with overwrite flags
and target/application versions.
An independent per-path oracle selects the first writer or last overwriting
writer by sequence, rather than folding the replay result. Invariants check
content agreement, shared-content survival, version checks, removal closure,
unique sequences, and stable unique UIDs. Structural history trimming commutes;
that check does not claim arbitrary game-specific compatibility or that every
removal request is independently acceptable.

Bounds: two components, five candidate revisions, three paths, and five successful
installs. Parser correctness, actual SHA-1/BLAKE3 bytes, firmware dependencies,
Wii IOS relationships, and shared filename encoding are covered by code review
and integration tests rather than this abstraction. Snapshot reinstalls may
repeat an inactive revision within ancestry; ordered histories reject already
active revisions. Each revision abstracts one distinct package version.

## Publication and recovery

`ComponentJournal` maps to `storage/git/component/transaction.rs` and
`storage/git/checkout.rs`. A prepared target becomes durable through the journal;
component refs publish individually, then the aggregate ref, checkout files,
index, and finally journal removal. Crashes or operation failures can occur
between these steps. Recovery accepts either the old or new value for each ref
and completes the same target. Another transition cannot replace a pending one.

The model checks agreement when settled, dirty-write detection, journal retention,
and eventual recovery under fair progress with bounded faults. Guest writes use
the same NAND lease as runtime install/uninstall. Explicit recovery may discard
dirty guest changes. Writing bytes equal to the existing committed bytes is not
considered dirty data protected from an intentional later install.

The primary run has two operations, one crash, one failure, and one guest write.
A second run uses two NANDs and no guest writes to check isolated publications
through the shared platform index. Successful object/ref writes are assumed
durable and ref replacement is atomic. Host power-loss ordering, external Git
writers, whole-NAND deletion, metadata parsing, and all filesystem errors are
outside the model. Recovery liveness assumes no permanent external ref conflict.

`PlatformIndex` separately models reading the platform index, replacing one NAND's
entries, and writing the merged index. Separate repository handles must share the
operation mutex. The previous per-handle mutex permits both writers to read the
old index and the second write to erase the first NAND's entries. The implementation
now shares a lock by canonical repository directory within the process. This is
not an interprocess lock.

## Volume deletion

`VolumeDeletion` maps to `storage/git/repository/deletion.rs`. A deletion ref
records intent before any volume, component, or pending-install reference is
removed. Recovery on repository open repeats reference removal, index cleanup,
and checkout removal, then clears the marker. Pending deletion blocks component
recovery and commits, so an interrupted install cannot restore the removed NAND.

The model includes two volumes, one deletion, and an outstanding component
transaction. References may disappear in any order. Crashes and transient errors
are represented by stuttering between durable steps; fair progress assumes those
errors eventually clear. It checks complete cleanup, preserved neighboring volumes,
no resurrection after deletion starts, enabled recovery, and eventual deletion. It does not model Git lock internals or power-loss ordering.
Library reconciliation is separately covered by integration tests: removal keeps
other copies, and startup reconciles stale artifact entries with recovered storage.

## Implementation audit

The audit on 2026-10-08 compared the transitions against these implementation paths:

| Model behavior | Implementation |
|---|---|
| Snapshot selection versus ordered ancestry; increasing sequence numbers | `component/revision.rs`: `contributions`, `revisions`; `component/snapshot.rs`: `install` |
| Snapshot replacement and ordered replay | `nand/composition.rs`: `retained_revision`; platform composers under `impls` |
| Removal dependency closure and rejection of incomplete requests | `component/operations.rs`: `affected`, `uninstall`, `retained_states` |
| Rollback restricted to component ancestry and valid composition | `component/operations.rs`: `restore`; `component/composition.rs`: `compose` |
| Journal before publication; old/new reference values; checkout/index before clear | `component/transaction.rs`: `transition`, `recover`, `recover_reference` |
| Deletion intent before reference removal; cleanup before marker removal | `repository/deletion.rs`: `delete`, `finish_deletion`, `recover_deletions` |
| Pending deletion blocks install recovery | `component/transaction.rs`: `recover`; `commit.rs`: `commit_scoped` |

Paths beginning with `component`, `repository`, or `commit` are under
`src/storage/git`. The remaining paths are under `src`.

The audit corrected three modeling mismatches: uninstall now requires the request
to include its full dependency closure instead of silently removing dependents;
snapshot removal consults only the selected revision; snapshot reinstall may
reuse an inactive version from its ancestry. Additional checks cover ordered
ancestry, valid selected compositions, complete/idempotent dependency closure,
old-or-new pending references, lease exclusion, and enabled recovery. All eight
positive configurations pass, totaling 35,561 explored states. All seven negative
controls produce their intended counterexamples.

The composition model intentionally reaches quiescent states at its installation
bound, so its deadlock check is disabled. Journal runs keep deadlock checking on;
journal and deletion models also check enabled recovery and eventual completion.
These are bounded protocol abstractions checked against the source, not a formal
refinement proof of Rust, SQLite, Git, or filesystem behavior. Partial publication
is permitted while a journal exists and must converge after recoverable failures.

## Checked results

All passing runs completed on 2026-10-08 using `scripts/tlc.ps1`.

| Configuration | Distinct states | Result |
|---|---:|---|
| `MCNandComponents.cfg` | 1,347 | All invariants and UID stability pass |
| `MCNandComponentsSnapshot.cfg` | 4,731 | Pass, including inactive-version reinstall |
| `MCNandComponentsShared.cfg` | 3,707 | Pass |
| `MCNandComponentsDependencies.cfg` | 3,633 | Dependency closure and explicit removal pass |
| `MCComponentJournal.cfg` | 3,446 | Safety and recovery liveness pass |
| `MCComponentJournalCrossVolume.cfg` | 18,675 | Pass |
| `MCPlatformIndex.cfg` | 9 | Preserved entries and eventual publication pass |
| `MCVolumeDeletion.cfg` | 13 | Cleanup, isolation, and eventual deletion pass |

Negative controls deliberately violate the corresponding guarantees:

| Configuration | Expected counterexample |
|---|---|
| `MCNandComponentsReordered.cfg` | Grouping writes by component changes overlapping PS3 content |
| `MCComponentJournalEarlyClear.cfg` | Journal cleared before index publication |
| `MCComponentJournalReplace.cfg` | A pending journal is replaced |
| `MCComponentJournalNoLease.cfg` | Checkout publication erases an acknowledged guest write |
| `MCPlatformIndexSeparateLocks.cfg` | A completed NAND's index entries disappear |
| `MCVolumeDeletionEarlyClear.cfg` | Deletion completes with orphaned index and checkout data |
| `MCVolumeDeletionResurrection.cfg` | Unguarded install recovery resurrects deleted references |

Run a configuration with, for example:

```powershell
pwsh -File scripts/tlc.ps1 crates/filesystem/polymer-consolestorage/tla/MCNandComponents.tla -Config MCNandComponentsShared.cfg
```

The 28 integration tests in `polymer-library/tests/{vita,nintendo,ps3,wii}_histories.rs`
exercise real repositories and encrypted PKG/WAD fixtures. Wii coverage includes
shared ownership, rollback, preserved save paths, stable allocations, IOS removal,
corruption/cancellation, reopening after interrupted checkout, and concurrent
installation through independent repository handles. Deletion coverage includes
reference/index locks, Windows checkout sharing violations, pending install
recovery, and catalog reconciliation while preserving other copies. Firmware history uses the
public component API rather than a real PUP fixture. These tests and finite models
do not establish hardware power-loss durability or emulator compatibility.

## Remaining integration work

- Component history and rollback are available through the Rust artifact registry.
  The runtime actor/JS bridge currently exposes history and restore for saves,
  not NAND component revisions; user-facing rollback still needs that wiring.
- The platform operation lock is shared within one process. Concurrent processes
  writing the same repository require an additional locking contract.
- Real emulator smoke tests, real PUP extraction, and host power-loss testing
  remain separate verification work.

`ARCHITECTURE.md` is human-owned and still describes package uninstall as Git
revert. It needs a human update to describe component histories and composition.
