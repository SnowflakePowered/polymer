# Checkout recovery verification (T14)

Session writes go directly into a Git volume's checkout. The list of pending changes exists only in memory, so after a process crash the checkout can differ from the last commit with no record of why. A crash after a completed save write cannot be distinguished from a crash during one, and a multi-file save can be left half written even when every file is complete. Recovery therefore rolls back to the last commit, which is the last known good state, and records the crashed checkout first so nothing is destroyed.

## Implementation

`GitVolumeStorage::ensure_checkout` runs once per volume per process. For every volume except a NAND with a tip, it compares the checkout against the last commit with `checkout_matches_commit`. On divergence it:

1. Records the checkout (`recovery.rs`, `preserve_checkout`). The tree keeps the committed `meta/`, replaces `files/` and `meta/fs/directories` with the checkout contents, and is committed with parents `[tip, previous recovered entry]`. One reference transaction publishes it to `refs/consoles/recovered/v2/<kind>/<id>`, which retains every entry, and to `refs/consoles/recovering/v2/<kind>/<id>`, which marks the unfinished recovery.
2. Rolls the checkout back with `checkout_target` and rewrites the index.
3. Deletes the `recovering` reference (`finish_recovery`). A stale marker is also cleared on the matching path.

A crash at any point resumes on the next open, because divergence is computed again. If a `recovering` entry exists, the current checkout is rebased onto it: paths whose content matches the entry or the commit keep the entry's content, and only other paths take the current content. No new entry is written when nothing changes. A partially rolled-back save is therefore never recorded. A missing checkout root is rebuilt without recording. While a composed-rename operation marker (`.git/polymer/operations/<id>/pending`) or a save-materialization journal exists, that protocol owns the checkout: `ensure_checkout` neither records nor rolls back and leaves the volume unready, so the check repeats on later accesses. Those protocols commit through `commit_pending`, which marks the checkout ready, so remaining crash leftovers are recovered on the next process start.

`PlatformRepository::volume` now returns a shared `GitVolumeInner` per canonical Git directory, kind, and identity while any handle is alive. Previously each call created an independent instance whose first access could roll back another instance's live session. Volume deletion removes both recovery references.

## Model

`CheckoutRecovery.tla` models two saves (`A = {a1, a2}`, `B = {b1}`), session writes with in-memory pending changes, checkpoints, clean stops, session crashes (optionally mid-write, leaving a torn file unless `Shadow`), crashes during recovery, index loss, external changes while stopped, and a second instance opening during a session. `Recovery` selects the current code, a NAND-style guard, keeping the crash state (`rescan`), or record-then-rollback (`preserve`). `Order` selects recording before or after rollback; `Dedupe` selects no dedupe, dedupe against all history, or dedupe against the unfinished recovery (`episode`); `Registry` selects one shared instance per volume.

Invariants: `RestartsFromLastGood`, `NoTornCurrent`, `NothingDestroyed` (every file recovery overwrote was recorded), `SavesPreserved` (every real whole-save state recovery overwrote was recorded), `EntriesExisted` (every recorded save state existed on disk), `PreservedComplete` (no torn file recorded), `ProfileTracksCheckout`, and `LiveSessionUndisturbed`.

Results with TLC on 2026-10-09:

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCCheckoutRecovery.cfg` (preserve, record first, episode dedupe, registry, shadow writes) | all pass | 1,972,723 |
| `MCCheckoutRecoveryTwoCrashes.cfg` | all pass | 221,521 |
| `MCCheckoutRecoveryNoShadow.cfg` | all except `PreservedComplete` pass | 7,567,543 |
| `MCCheckoutRecoveryNoShadowPreserved.cfg` | `PreservedComplete` violated | — |
| `MCCheckoutRecoveryRollbackFirst.cfg` | `NothingDestroyed` violated | — |
| `MCCheckoutRecoveryNoDedupe.cfg` | `EntriesExisted` violated | — |
| `MCCheckoutRecoveryHistoryDedupe.cfg` | `SavesPreserved` violated | — |
| `MCCheckoutRecoveryNoRegistry.cfg` | `LiveSessionUndisturbed` violated | — |
| `MCCheckoutRecoveryCurrent.cfg` | `NothingDestroyed` violated | — |
| `MCCheckoutRecoveryCurrentTorn.cfg` | `NoTornCurrent` violated | — |
| `MCCheckoutRecoveryCurrentProfile.cfg` | `ProfileTracksCheckout` violated | — |
| `MCCheckoutRecoveryCurrentNoRegistry.cfg` | `LiveSessionUndisturbed` violated | — |
| `MCCheckoutRecoveryRescan.cfg` | `NoTornCurrent` violated | — |

The current-code traces show a crash during a write leaving a torn save current when no committed file is missing, while a deleted save makes the same restart discard every uncommitted change. A second instance opening during a session restores a save the running game deleted.

## Correspondence and limits

The Rust records one volume-level snapshot rather than one entry per save; each save's projection of a snapshot is a state that existed, which is what the per-save properties require. Dedupe compares against the latest `recovering` entry rather than every entry of the unfinished recovery; after an entry is recorded, rollback only moves files toward the commit, so the latest entry covers every partial state. `SavesPreserved` excludes per-file combinations of an unfinished recovery's entry and the commit, which remain covered by `NothingDestroyed`.

The deferral to other journals is not modeled; during it, and for the rest of that process, a torn file left beside the journaled operation can stay current. Shadow writes are not implemented, so a torn file can be recorded. Recovered entries are not yet shown in the save-history UI. Atomic rename, fsync ordering, and Git reference durability under power loss are assumed. Multiple processes sharing a repository are outside the model.

On 2026-10-09 the `polymer-consolestorage`, `polymer-vfs`, `polymer-runtime`, and `polymer-library` suites pass, including the six composed-recovery tests that hold a rename journal across reopen. Two new public integration tests in `tests/checkout_recovery.rs` pass: a reopened PS1 card after simulated crash edits is restored to its commit with the modified, deleted, and created saves recorded; and a second handle opened during a session keeps that session's uncommitted deletion and creation. The second test fails without the shared registry.
