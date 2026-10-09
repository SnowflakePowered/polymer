# Managed resource leases (T11)

The implementation gives each materialization scope one finalization authority. Factories declare exclusive keys, which `MaterializationScope::build` acquires before calling the factory, and register cleanup in that scope. Prepared filesystems and derived entries build directly into the enclosing scope. Storage finish, final checkpoint, and watcher join must succeed before any of its keys are released.

## Ownership and retry implementation

`Materialized::lease` is an owned, non-cloneable `ManagedResourceLease`. Readable plugin handles (packages, caches, save-state scratch directories, quick resume) own their resource for the handle's lifetime. `SharedResourceFactory` materializes the source once into its own scope and keeps the lease behind an `Arc`. Reads use the cached backend directly, with no scope, lock acquisition, or cleanup worker. A launch that mounts the handle stores a clone of that `Arc` in its scope with `MaterializationScope::retain`. Retained leases are only dropped, never finalized explicitly, so cleanup starts when the last holder (the plugin's handle or a launch scope that has finished its own cleanup) drops it. Storage handles are not readable and are not shared: each launch materializes them exclusively, and `ManagedResourceLeases.tla` covers them.

Each scope reserves a dedicated cleanup worker before acquiring keys or creating backends. Prepared filesystems also reserve a worker before materialization for namespace adoption and enclosing finalization. Failed startup returns an error before resources are admitted. Workers wait for their one ownership transfer; dropping the sender after successful explicit cleanup stops the idle worker. This replaces the previous global queue with reserved workers, avoiding a second fallible thread spawn after acquisition. It costs one idle thread per standalone scope, plus an enclosing worker per prepared filesystem.

Drop transfers pending state to its reserved worker without running cleanup on the dropping thread. The worker retries sequentially within its own scope. A hung finalizer keeps that scope's locks and occupies only its worker. Unrelated workers continue. The existing two-second delay is retry backoff after a returned error, never a deadline for an in-flight operation. There are no timeout-based releases or overlapping attempts. A worker panic retains its state permanently rather than releasing unverified resources; panic recovery and process crashes are outside the progress model.

## Models and Rust correspondence

`ManagedResourceLeases.tla` covers acquisition, partial rollback, construction failure, cleanup ordering, retained retries, and competing storage jobs. Two clients, one job, two keys, two cleanup steps and two failures produce 1,514 distinct states. All configured safety and progress properties pass. Its early-release negative control violates `WatcherProtected`.

`SharedHandleResources.tla` checks handle-owned sharing. A plugin handle and two launches hold references to one readable resource; launches mount, write, and finish; the handle reads up to three times and is dropped; an external party competes for the path key; cleanup fails up to twice. With `Design = "refcount"` (`MCSharedHandleResources.cfg`), `TypeOK`, `KeyProtected`, `ReferencedIsLive`, `ActiveLaunchHeld`, `HandleHeld`, `CleanupUnreferenced`, `OneInstancePerHandle`, `ReadNotRejectedByOwnMount`, `ReadSeesWrites`, `AllReleased`, and `AllDone` pass across 1,295 distinct states. A read during an active launch that has written, and two launches sharing one instance, are both reachable. `Design = "weak"` models the previous weak-identity protocol: `MCSharedHandleResourcesWeakShared.cfg` violates `ReadNotRejectedByOwnMount` (an `onQuit` save-state read while the launch holds the scratch directory), `MCSharedHandleResourcesWeakLost.cfg` violates `ReadSeesWrites` (a read after the launch finished gets a fresh empty directory), and `MCSharedHandleResourcesWeakInstances.cfg` violates `OneInstancePerHandle`.

`ReservedFinalizationWorkers.tla` checks reservation before acquisition, startup rejection, retained work, bounded cleanup errors, and one in-flight attempt per scope. The healthy configuration passes all invariants and `AllFinish` across 100 distinct states. The hang configuration lets only `save` hang: all invariants and `HealthyScopesFinish` pass across 120 states, including schedules where `other` finishes while `save` remains hung and locked. Startup-aborted scopes count as terminal without acquiring resources.

| Model transition or state | Rust correspondence |
|---|---|
| Worker `Reserve` / `StartupFailure` | `FinalizationRetry::reserve` starts the receiving thread, or returns the spawn error before acquisition. |
| `Acquire` / partial rollback | `MaterializationScope::build` acquires a factory's keys before calling it; collecting guards rolls back that acquisition group on failure. Earlier registered resources remain in the enclosing scope. The model acquires all keys before any registration, while a prepared filesystem interleaves them per resource; each resource's keys are still held before its own cleanup is registered. |
| Shared `Materialize` / `Read` | `SharedResourceFactory::shared` materializes once under its mutex and returns the cached backend and lease; `FilesystemState::volume_backend` and `file_backend` serve reads from it. |
| Shared `Mount` / `Finish` | `SharedResourceFactory::materialize_in` calls `MaterializationScope::retain`; the launch's reference is dropped with its ownership after its own cleanup succeeds. |
| `DropHandle` / `BeginCleanup` | Dropping `FilesystemState` drops the handle's `Arc`; the last drop runs `ManagedResourceLease::drop`, which hands pending cleanup to its reserved worker. |
| `Build`, `Register` | Factory `materialize_in` and `MaterializationScope::push` register cleanup before subsequent fallible work. |
| `Ready` | `into_lease` transfers the unique scope state into its owning lease. |
| Construction abort | Dropping the scope's lease latches its `LaunchFailed` fallback and transfers state to the reserved worker. |
| `Finalize` | Managed finalization uses a mutex and latches the first reason with `get_or_insert`; prepared finalization also latches its reason before namespace adoption. Reason latching is not modeled and rests on this code review. |
| `Cleanup` / cursor | A queue entry is removed only after its finalizer returns `Ok`. |
| Worker `Start`, `Fail`, `Finish` | The reserved worker receives state once and retries on returned errors. Explicit finalization follows the same serialized cleanup operation; after drop, the worker is the sole caller. |
| Watcher shutdown | `SaveWatchingLease::stop` waits for the final checkpoint reply and joins before succeeding. A failed checkpoint leaves the watcher available for another request. |
| `Release` | `FinalizationState::finalize` clears guards only after the entire queue succeeds; removing state expires the scope identity. |
| `Hang` | A call that never returns retains its worker, pending state, and keys. No other worker needs that execution thread. |

These are bounded protocol models and a manual correspondence review, not a machine-checked refinement proof. The sharing model handles one readable resource, one handle, and two launches; reads are atomic, and the weak-design read's transient cleanup is modeled as immediate, which favors the weak design. The worker model abstracts ownership transfer and synchronous explicit cleanup into the same serialized execution states. Namespace adoption and backend I/O internals are not modeled here. Progress assumes fair scheduling and finitely many returned cleanup errors for a non-hanging scope. Host path aliasing, writes through backends after finalization, and process crashes are outside these models. Frontend early returns using `Cancelled` rather than `LaunchFailed` remain a separate issue.

## Historical counterexamples

The original `ResourceLeases.tla` / `MCResourceLeases.cfg` are preserved. The following focused models also retain the pre-fix protocols and their counterexamples; they do not describe the updated Rust behavior.

`ScopedDependencies.tla` / `ScopedDependencies.cfg` modeled the intermediate weak-identity protocol that rejected attachment from unrelated live scopes; it passed across 276 states but did not model plugin reads, which `SharedHandleResources.tla` shows that protocol rejects or loses.

`SharedResourceLeases.tla` modeled two parent scopes sharing an independently finalizable managed dependency. One parent could release it while the other still needed it, and an owning alias could release it even under a common root.

| Configuration | Historical result | Distinct states |
|---|---|---:|
| `MCSharedResourceLeases.cfg` | Common root passes safety and eventual release | 49 |
| `MCSharedResourceLeasesIndependent.cfg` | `DependencyProtected` violated | 24 |
| `MCSharedResourceLeasesAlias.cfg` | `DependencyProtected` violated | 8 |

`ResourceLeaseRetries.tla` modeled the global queue and single sequential worker. Failed startup waited for a later enqueue to try again. A hung finalizer blocked every remaining entry.

| Configuration | Historical result | Distinct states |
|---|---|---:|
| `MCResourceLeaseRetries.cfg` | Healthy safety and eventual release pass | 87 |
| `MCResourceLeaseRetriesSpawnSafety.cfg` | Safety and conditional progress pass | 17 |
| `MCResourceLeaseRetriesSpawnFailure.cfg` | `AllReleased` violated | 17 |
| `MCResourceLeaseRetriesLaterEnqueue.cfg` | Later enqueue restores progress | 123 |
| `MCResourceLeaseRetriesHangSafety.cfg` | Safety and conditional progress pass | 101 |
| `MCResourceLeaseRetriesHang.cfg` | `AllReleased` violated | 101 |
| `MCResourceLeaseRetriesHangBlocksOthers.cfg` | `UnhangingLeasesReleased` violated | 101 |

## Validation

```powershell
$env:TLC_WORKERS = '2'
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/MCManagedResourceLeases.tla
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/SharedHandleResources.tla -Config MCSharedHandleResources.cfg
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/ReservedFinalizationWorkers.tla
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/ReservedFinalizationWorkers.tla -Config ReservedFinalizationWorkersHang.cfg
cargo test -p polymer-vfs -p polymer-runtime --test memory_card_writeback --test save_watcher_leases
cargo check -p polymer-runtime
```

On 2026-10-09, all six integration tests and `cargo check -p polymer-runtime` passed after this refactor. The five public VFS writeback integration tests cover real storage behavior. The runtime test locks a real Wii profile's Git ref during the final checkpoint, verifies that both profile and NAND remain unavailable across repeated finalization errors and filesystem drop, then removes the obstruction and waits for background cleanup to release both keys. It reopens the profile, verifies saved bytes, and confirms that no checkpoint remains pending. No mocks or test-only production interfaces are used.

The runtime check covers plugin-host and Windows platform adapters. The macOS bundle adapter is updated and formatted, but cannot be compiled on this Windows host.
