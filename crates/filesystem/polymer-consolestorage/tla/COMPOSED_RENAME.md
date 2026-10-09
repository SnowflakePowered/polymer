# Composed rename verification (T12)

The revised model separates pre-fix implementation counterexamples from a proposed protocol. Results below were checked with TLC 2026.10.06 on 2026-10-08 and re-run on 2026-10-09; they supersede the earlier 7,749,970-state run, whose key-layer durability assumption was incorrect.

On 2026-10-09, the handle and operation-locking changes were implemented in `src/storage/composed/`. Shared bindings retain replacement backing handles, each open retains its access rights, and directory moves rekey descendant registrations. Removal and atomic installation detach old registrations. Memory `create()` now truncates an existing object, matching native storage. Async installation spools to a private temporary file before acquiring the composition gate for publication.

Nine public integration tests in `tests/composed.rs` cover round trips, access rights, descendant registration, replacement, unlink/recreation, truncation, concurrent writes, slow install input, and failed source removal. The composition gate coordinates its namespace operations and file handles. Recursive enumeration takes the gate per directory, not for an entire walk.

Internal layer descriptors now distinguish volatile layers, Git profiles, and unmanaged storage supplied through the existing public constructor. Console compositions supply stable layer labels and retain the exact save repository's backing volume and versioning object. Rename commits the pending changes of each affected Git persistence domain once; path-based composition sync also commits its domain. Git path sync registers the file in pending changes so writes through retained handles after a prior commit can be included. Standalone Git sync still does not commit, and handle-only sync does not invoke the composition adapter.

Rename and path sync acquire the same profile operation mutex as checkpoint and restore, shared by canonical repository location and profile identity across versioning instances. An active profile operation or pending save-materialization journal causes an error before rename changes the namespace; synchronous calls do not block an async runtime waiting for that mutex. The profile's shared operation object holds an in-process recovery flag. Handle I/O and namespace reads check only that flag. Namespace mutations and history operations check the on-disk record and refresh the flag. A record written by another process is therefore noticed by reads only after the next mutation, recovery, or mount. Raw backend access is outside the coordination contract. A storage can be mounted only once per plugin session (`FilesystemPlan::seal` rejects a second binding), and the mounted session holds its exclusive lock, so other sessions and storage operations cannot open a second live composition of the profile; `launch_storage` still composes before a launch acquires that lock, so a competing launch can run composition recovery before it fails to mount. Two real-storage Wii tests verify persisted cross-layer publication/removal, pending-edit inclusion, retained-handle writes followed by path sync, and refusal during an interrupted restore.

Cross-layer file renames involving the console composition's Git save profile now use journaled recovery. A private record and checksummed payload live under `.git/polymer/operations/<profile>/`, outside the checkout. The prepared record precedes the payload; the payload is synced before an atomic committed-record replacement. Publication and source removal precede committing the profile's pending changes. Only then is the record removed and the live handle binding moved. Failures retain the record and block composed I/O and history mutations. Recovery runs before exposing a composition and on `sync`, `finish`, or explicit `recover`; prepared records abort, while committed records roll forward. A new composition session skips deletion of a regenerated volatile source.

Six public tests in `polymer-library/tests/composed_recovery.rs` exercise failed Git ref publication and retry with live handles, remount recovery with regenerated scratch, persistent source removal into volatile scratch, corrupted payload rejection and repair, prepared-record abort, and invalid-destination rejection before committing the journal. Together with nine composed tests and thirty console history tests, all 45 pass on 2026-10-09. These use real storage and Git lock failures, not injected mocks or process-kill tests.

The implementation supports one persistent profile per composition. Unmanaged public-constructor layers, volatile-only transfers, and same-layer renames do not use this journal. A cross-layer rename without the journal stages the copy under its private name, removes the source, and then publishes. If removal fails, the staged copy is deleted and the error is returned. If publication fails, the source is recreated from the staged copy and its live handles are rebound before the error is returned. This replaces the ignored removal error behind `CleanupFailure`. No public storage fails the publishing rename once the destination is validated, so the restoration branch is model-checked but not integration-tested. If restoration also fails, the file loses its name (`TransferDoubleFault`). A crash between removal and publication loses the source on a durable unmanaged layer; console compositions use this path only between volatile layers. Destination staging still uses a named backend file with a stable transaction identity; the composition gate and pending marker hide it until recovery completes. Direct backend writers are outside this contract; a second live composition is excluded as described above, apart from the pre-lock composition in `launch_storage`. File sync and atomic record replacement do not establish host power-loss durability or Git directory/ref fsync ordering. The model results remain a bounded protocol check, not a Rust refinement proof.

## Storage semantics

The usual composition contains a key projection, optional saves, and scratch. Only saves retain guest mutations across the process crash modeled here. `KeyStoreStorage` regenerates key files from backing data; its guest writes and pending namespace changes are in memory. Scratch guest-created files disappear. The initial scratch file `t` represents such a guest-created file, not a file regenerated by a scratch factory.

`InitNs` and `ResetNs` distinguish the running namespace from the reconstructed namespace. A regenerated key gets a fresh logical identity `(object, restart epoch)`. Seeing both that fresh projection and an earlier copy in saves after restart is not a duplicate surviving file instance. It also does not mean guest edits to the key survived. The generic `Cleanup` configuration deliberately makes a second backend persistent: its label remains `keys`, but this is not the current key projection's durability contract.

Successful persistent operations are assumed to survive a process crash. Hardware power loss, fsync ordering, Git checkpoint publication, and the asynchronous save worker are outside this model.

The 2026-10-09 recovery audit found that this persistence assumption is stronger than the actual Git save backend. `GitVolumeStorage::sync` flushes the checkout file, without committing the profile or its pending namespace changes. `ensure_checkout` can reconstruct a profile from the committed tree when an indexed path is missing. A real-storage probe committed `from = source` and `to = old destination`, successfully renamed `from` over `to`, synced `to`, dropped the repository/volume handles, and reopened them. Reopen restored `from` and the old destination. Native directory storage retained the rename in the same reopen control. This was a fresh-open test, not a process-kill or power-loss test. A recovery journal cannot be cleared on the strength of the current Git `sync` operation; it needs confirmation that the reconstructible profile state includes the operation. The passing model configurations assume such a persistence boundary and do not verify its implementation.

## Pre-fix implementation observations

| Scenario | Observation |
|---|---|
| Copy/write race | An acknowledged write after the source copy and before binding movement reaches the old object and is absent from the destination. No crash is required. |
| Parent directory rename | After `t -> d/f`, renaming `d -> e` leaves the child's redirect at `d/f`. Recreating `d/f` sends later writes to an unrelated file; without recreation, access fails. |
| Return to original path | After `t -> d/f -> t`, redirect equality with the original path selects the original, detached native handle. Writes succeed on an object no longer named by `t`. |
| Published destination / source cleanup | Ignoring source-removal failure can leave two names for the copied logical file even with volatile sources, while the process is running. A crash leaving both original instances requires both backends to retain them; that is the generic persistent-backend case, not the normal key projection. |
| Staging crash | A crash after the copy into saves but before publication leaves a guest-visible `.f.polymer-<uuid>` file. The model uses `~f` as its finite representative. |
| Read-only handle | Redirected access reopens with read/write access; a previously read-only handle can write after rename. |
| Destination replacement | A redirected handle resolves its path again on every operation, so replacing that path can switch the handle to an unrelated file. |

Replacement means installing a new object at an existing name, as with atomic installation. It does not model `DirectoryStorage::create` truncating an existing native object in place. Memory `create()` now also truncates in place. Intentionally displaced handles remain attached to their old object; their writes are not required to appear at a path now naming another object.

## Proposed protocol

Three switches distinguish the proposal from current behavior:

| Switch | Required behavior |
|---|---|
| `LockIO` | Serialize guest operations with the whole rename and recovery sequence. The model uses one global operation gate, including namespace mutations and handle writes. |
| `StableHandles` | Shared bindings hold backing objects rather than reopening redirect paths. Preserve each handle's access rights, detach replaced path registrations, and rekey descendant registrations on directory rename. |
| `Journaled` | Keep a private staged payload and transaction record, with an explicit commit decision and idempotent recovery. |
| `RemoveFirst` | Without a journal, remove the source before publishing the staged copy. Removal failure aborts; publication failure restores the source from the staged copy and rebinds its handles. |

Cross-layer rename proceeds through prepare, private copy, commit decision, destination publication, source cleanup, binding movement, and journal removal. Before the decision, failure or restart aborts without changing a persistent destination. After the decision, failure retains the transaction and recovery rolls forward. Cleanup errors are retried. Recovery completes before guest operations resume. Same-layer native rename remains an atomic namespace step under the same operation gate, followed by binding updates.

The journal and staged payload must survive restart, including when the destination is volatile. Publication must be repeatable from that retained payload. Cleanup must identify the original source instance and must not delete a newly regenerated projection at the same path. The implementation retains a private payload and uses composition session identities for volatile sources. Its binding update follows journal removal while the operation gate is still held; the model places binding movement before removal. Restart discards live handles in both cases.

The ghost variables `identity`, `cur`, `acked`, and `published` track obligations and prevent accounting for the same recovered publication twice. Actual recovery publication is unconditional and repeatable; it does not depend on the ghost publication flag. After a completed move into volatile scratch, a subsequent crash may discard that file. Journaling does not make scratch permanently durable.

## Checked properties and bounds

Every proposed configuration checks `TypeOK`, `AckedWritesVisible`, `HandleFollowsFile`, `OpenHandleIdentity`, `NoDuplicateFile`, `NoStagedLeftover`, `WritesRespectAccess`, `PreparedDestinationUnchanged`, `JournalRetained`, and `RecoveryEnabled`, plus temporal property `RenameSettles`.

File/namespace consistency is required at settled states. Proposed serialization prevents guest operations from observing intermediate states. Acknowledged-write visibility applies while the logical file still has a name; after crash it applies only to files assigned to persistent storage, including committed transactions awaiting recovery. Writes are unique tokens rather than byte ranges. Access-right checking records any successful unauthorized write. Progress assumes weak fairness and bounded faults; it makes no promise under permanent I/O failure.

All configurations use three layer labels and nine paths. Copy, staging, cleanup, read-only, and projection cases use four object slots, one handle, and one rename. Directory and replacement use five slots; replacement uses two handles. Directory and return allow two renames. Write cases allow two acknowledged writes; staging, cleanup, and projection use zero. Proposed cases allow one crash and one failure, except `ProposedCleanupFailure`, which allows no crash. Current directory, return, and replacement cases enable `LockIO` solely to isolate their handle defect from the independent copy race; they do not claim the implementation already serializes I/O. Directory creation also explores renaming over an existing destination and preserving it on precommit abort.

Config names below have prefix `MCComposedRename` and suffix `.cfg`; `(base)` has no intervening suffix. Counterexample counts are states explored before the violation, not exhaustive successful checks; these counts can vary with worker scheduling.

| Current configuration | Result | Distinct states |
|---|---|---:|
| (base) | `AckedWritesVisible` violated | 36 |
| Directory | `AckedWritesVisible` violated | 224 |
| Return | `AckedWritesVisible` violated | 66 |
| Staging | `NoStagedLeftover` violated | 14 |
| Cleanup | `NoDuplicateFile` violated; generic persistent source | 39 |
| CleanupFailure | `NoDuplicateFile` violated; volatile key source, no crash | 24 |
| ReadOnly | `WritesRespectAccess` violated | 13 |
| Replacement | `OpenHandleIdentity` violated | 115 |
| Projection | `TypeOK`, `NoDuplicateFile` pass; actual key restart behavior, no I/O failure | 40 |
| TransferUnordered | `NoDuplicateFile` violated; volatile keys to scratch without `RemoveFirst` | 99 |

| Proposed configuration | Result | Distinct states |
|---|---|---:|
| Proposed | All configured properties pass | 200 |
| ProposedDirectory | All configured properties pass | 2,451 |
| ProposedReturn | All configured properties pass | 984 |
| ProposedStaging | All configured properties pass | 86 |
| ProposedCleanup | All configured properties pass | 117 |
| ProposedCleanupFailure | All configured properties pass | 34 |
| ProposedReadOnly | All configured properties pass | 86 |
| ProposedReplacement | All configured properties pass | 4,068 |
| ProposedProjection | All configured properties pass | 141 |
| Transfer | All configured properties pass; volatile keys to scratch, `RemoveFirst` | 221 |
| TransferDoubleFault | `HandleFollowsFile` violated; publication and restoration both fail | 175 |

Run from the repository root, choosing a configuration from the tables:

```powershell
$env:TLC_WORKERS = '2'
./scripts/tlc.ps1 crates/filesystem/polymer-consolestorage/tla/MCComposedRename.tla -Config MCComposedRenameProposed.cfg
```

The configurations split scenarios to keep counterexamples understandable; they do not exhaustively combine all operations. Ownership claims are fixed. Dynamic claims, hard links, partial byte copies, truncation, flush semantics, interprocess writers, journal-record encoding/corruption, and host filesystem failure semantics are not checked. Reads and listings are represented by settled-state namespace/handle invariants, not independent actions. Private staging cleanup is abstracted by dropping its retained reference, not by a host-file garbage collector. Passing this finite design model is not a refinement proof of Rust code.

## Payload corruption

`MCComposedRenameCorruption.tla` extends the existing model without changing the historical configurations. It adds a payload-integrity bit, a bounded corruption action, an external repair action, and a publication guard corresponding to `RenameJournal::payload`. Corruption can occur after copying, before the commit decision or before any publication attempt, including replay after a crash. It survives restart until the payload is repaired or its prepared transaction is aborted. Repair means restoring the original payload; the model does not assume the application can reconstruct it automatically.

The integrity bit abstracts both length and BLAKE3 mismatch. It is separate from destination data: damaging the retained payload after an earlier successful publication must not damage that already-published destination. `NoCorruptPublication` records whether any publication used an invalid payload. `CorruptionBlocked` requires a committed journal, blocked guest access, and disabled publication while validation cannot pass. `RecoveryOrBlocked` replaces the original unconditional `RecoveryEnabled`: waiting for repair is an intentional blocked state. `RecoveryAfterRepair` checks eventual settlement conditional on integrity eventually remaining valid. `RepairedSpec` adds weak fairness for external repair and checks unconditional settlement under that extra assumption.

Checked with TLC on 2026-10-09, using the replacement scenario with two handles, five object slots, two writes, one rename, one crash, one I/O failure, and one corruption:

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCComposedRenameCorruption.cfg` | Safety and conditional recovery pass; repair is optional | 8,221 |
| `MCComposedRenameCorruptionRepaired.cfg` | Safety and eventual settlement pass with fair external repair | 8,221 |
| `MCComposedRenameCorruptionUnchecked.cfg` | Expected `NoCorruptPublication` violation with validation disabled | 245 |
| `MCComposedRenameCorruptionPermanent.cfg` | Expected `RenameSettles` violation without guaranteed repair | 8,221 |

The unchecked counterexample copies, commits, corrupts, and publishes. The permanent-corruption counterexample retains a committed transaction at publication and stutters forever without repair. Both negative controls ensure the positive results do not imply recovery from permanently damaged bytes. Counterexample state counts can vary with worker scheduling.

```powershell
$env:TLC_WORKERS = '2'
./scripts/tlc.ps1 crates/filesystem/polymer-consolestorage/tla/MCComposedRenameCorruption.tla -Config MCComposedRenameCorruption.cfg
```

The corresponding public Rust test, `corrupt_payload_blocks_recovery_until_the_payload_is_repaired`, checks both a length mismatch and a same-length byte change. Each failed mount retains the journal, blocks checkpoints, and leaves the previously published destination intact. Restoring the original payload permits recovery. All six `composed_recovery` tests pass after this extension.

Validation and publication are one abstract step. The Rust implementation hashes, rewinds, then copies the private payload; concurrent mutation between those reads is outside this model and the implementation's coordination contract. Hash collisions, a corrupt expected digest, and journal-record corruption are not modeled. This checks the protocol assuming reliable integrity detection, not the BLAKE3 algorithm or an exact refinement of Rust execution.
